using System.Text;
using System.Text.Json;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// Recordings, part 1 (minor 5): the events of docs/FFI-RECORDINGS.md
/// section 8 as the core writes them, and FakeRecordings from configure to
/// the Recycle Bin. Synthetic device keys only.
/// </summary>
internal static class RecordingsTests
{
    private const string Device = "d-3f9a0c1b7e2d4a65";

    private static RecordingsEvent? Parse(string json) => RecordingsEvent.Parse(Encoding.UTF8.GetBytes(json));

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

    public static void Run()
    {
        var cam = new RecordingCamera(Device, 3);

        T.Run("recordings: index events", () =>
        {
            T.Equal(new RecordingsEvent.Index(Device, "busy", 11, 40),
                Parse($$"""{"type":"recordings_index","device":"{{Device}}","state":"busy","pending_days":11,"revision":40}"""));
            T.Equal(new RecordingsEvent.DayFailed(Device, cam, 1791504000, "session closed during the search"),
                Parse($$"""{"type":"recordings_day_failed","device":"{{Device}}","camera":{"device":"{{Device}}","channel":3,"lens":0},"day":1791504000,"reason":"session closed during the search"}"""));
            T.Equal(new RecordingsEvent.Changed(Device, 41, 1791590400, 1791676799),
                Parse($$"""{"type":"recordings_changed","device":"{{Device}}","revision":41,"start":1791590400,"end":1791676799,"future":true}"""));
            T.Equal(new RecordingsEvent.IncidentsChanged(12), Parse("""{"type":"incidents_changed","revision":12}"""));
            T.Equal(new RecordingsEvent.CameraGroupsChanged(3), Parse("""{"type":"camera_groups_changed","revision":3}"""));
            T.Check(Parse("""{"type":"recordings_changed","device":"d-1"}""") is null);
            T.Check(Parse("""{"type":"incidents_changed"}""") is null);
            T.Check(Parse("""{"type":"device_state","device":1,"state":"connected"}""") is null);
            T.Check(Parse("{") is null);
        });

        T.Run("recordings: thumbnail events and keys", () =>
        {
            var key = new ThumbnailKey(cam, "sub", 1791618125, 1791618141);
            T.Equal(new RecordingsEvent.Thumbnail(key, "ready", null),
                Parse($$"""{"type":"thumbnail","key":{"camera":{"device":"{{Device}}","channel":3,"lens":0},"tier":"sub","start":1791618125,"end":1791618141},"state":"ready"}"""));
            RecordingsEvent.Thumbnail failed = T.NotNull(Parse("""{"type":"thumbnail","key":{"camera":{"device":"d-1","channel":0},"start":1,"end":2},"state":"failed","reason":"not added"}""")
                as RecordingsEvent.Thumbnail);
            T.Equal("sub", failed.Key.Tier);
            T.Equal("not added", failed.Reason);
            using JsonDocument doc = JsonDocument.Parse(key.Json);
            T.Equal(key, ThumbnailKey.From(doc.RootElement));
            T.Equal($$"""{"device":"{{Device}}","channel":3,"lens":0}""", cam.Json);
        });

        T.Run("recordings: download events", () =>
        {
            var e = Parse($$"""{"type":"download","download":"1791618125-9c1e5a2b7d40f316","incident":"i-x","folder":"/downloads/x — Camera 1","state":"running","paused":false,"cameras":[{"camera":{"device":"{{Device}}","channel":3,"lens":0},"name":"Camera 1","state":"running","paused":false,"chunks_done":1,"chunks_total":2,"bytes":20971520,"bytes_estimate":41943040,"output":null},{"camera":{"device":"{{Device}}","channel":5,"lens":0},"name":"Camera 2","state":"failed","paused":false,"chunks_done":0,"chunks_total":2,"bytes":0,"reason":"stalled","detail":"no data for 10 s (attempt 8 of 8)","output":null}]}""")
                as RecordingsEvent.Download;
            DownloadProgress d = T.NotNull(e).Progress;
            T.Equal("running", d.State);
            T.Equal(2, d.Cameras.Count);
            T.Equal(1, d.Cameras[0].ChunksDone);
            T.Equal(41943040L, d.Cameras[0].BytesEstimate);
            T.Check(d.Cameras[0].Output is null);
            T.Equal("stalled", d.Cameras[1].Reason);
            var shortForm = Parse("""{"type":"download","download":"a","state":"done","cameras":[]}""") as RecordingsEvent.Download;
            T.Equal(0, T.NotNull(shortForm).Progress.Cameras.Count);
            T.Equal(new RecordingsEvent.DownloadRemoved("a", false, null), Parse("""{"type":"download_removed","download":"a","trash":false}"""));
            T.Equal(new RecordingsEvent.DownloadRemoved("a", true, "/d/x"), Parse("""{"type":"download_removed","download":"a","trash":true,"folder":"/d/x"}"""));
        });

        T.Run("recordings: device_info flags", () =>
        {
            T.Equal(new RecordingSupport(true, true, true, true), RecordingSupport.FromDeviceInfo(Encoding.UTF8.GetBytes(
                """{"type":"device_info","device":1,"key":"d-1","name":"NVR","recordings":{"search":true,"playback":true,"download":true,"thumbnails":true}}""")));
            T.Equal(new RecordingSupport(false, false, false, false),
                RecordingSupport.FromDeviceInfo(Encoding.UTF8.GetBytes("""{"type":"device_info","device":1}""")));
        });

        T.Run("fake recordings: from configure to the Recycle Bin", () =>
        {
            var events = new List<RecordingsEvent>();
            var fake = new FakeRecordings(json =>
            {
                if (RecordingsEvent.Parse(Encoding.UTF8.GetBytes(json)) is { } e)
                {
                    events.Add(e);
                }
            })
            { RecordingWorkIsManual = true };
            IRecordings rec = fake;
            const long day = 1791590400;

            T.Equal(NvStatus.NotConfigured, StatusOf(() => rec.RecordingsEnable(Device, "{}")));
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => rec.RecordingsConfigure("{")));
            rec.RecordingsConfigure("""{"downloads_dir":"/downloads"}""");
            rec.RecordingsEnable(Device, """{"channels":[{"channel":0},{"channel":1}]}""");
            rec.SoundConfigure("""{"alarms":true}""");
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => rec.SoundConfigure("""{"loudness_history_s":-1}""")));

            string cams = $$"""[{"device":"{{Device}}","channel":0},{"device":"{{Device}}","channel":1}]""";
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => rec.RecordingsWant($$"""{"cameras":[{"device":"{{Device}}","channel":7}],"days":[{{day}}]}""")));
            string many = string.Join(",", Enumerable.Range(0, 32).Select(i => day - i * 86400));
            T.Equal(NvStatus.Limit, StatusOf(() => rec.RecordingsWant($$"""{"cameras":{{cams}},"days":[{{many}}]}""")));
            rec.RecordingsWant($$"""{"cameras":{{cams}},"days":[{{day + 100}}],"force":false}""");
            T.Check(events.Contains(new RecordingsEvent.Changed(Device, 1, day, day + 86399)));
            T.Check(events.Contains(new RecordingsEvent.Index(Device, "idle", 0, 1)));
            T.Check(events.OfType<RecordingsEvent.IncidentsChanged>().Any());

            string query = $$"""{"start":{{day}},"end":{{day + 86399}}}""";
            using (JsonDocument e = JsonDocument.Parse(rec.RecordingsEvents(query)))
            {
                T.Equal(4, e.RootElement.GetProperty("events").GetArrayLength());
            }
            T.Check(rec.RecordingsCalendar(query).Contains("\"days\":[", StringComparison.Ordinal));
            T.Check(rec.RecordingsSpans(query).Contains("\"truncated\":false", StringComparison.Ordinal));
            T.Check(rec.RecordingsStatus().Contains(Device, StringComparison.Ordinal));

            rec.RecordingsHoldToday(7, $$"""{"cameras":{{cams}}}""");
            T.Equal(2, fake.Holds[7].Count);
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => rec.RecordingsHoldToday(0, null)));
            rec.RecordingsHoldToday(7, null);
            T.Check(!fake.Holds.ContainsKey(7));

            // Incidents, newest first; one by key; a rename.
            string incident;
            using (JsonDocument doc = JsonDocument.Parse(rec.IncidentsGet(query)))
            {
                JsonElement list = doc.RootElement.GetProperty("incidents");
                T.Equal(4, list.GetArrayLength());
                incident = list[3].GetProperty("key").GetString()!;
            }
            T.Equal(NvStatus.UnknownId, StatusOf(() => rec.IncidentGet("i-unknown")));
            T.Check(rec.IncidentEdit($$"""{"op":"rename","key":"{{incident}}","name":"Delivery"}""").Contains(incident, StringComparison.Ordinal));
            using (JsonDocument doc = JsonDocument.Parse(rec.IncidentGet(incident)))
            {
                T.Equal("Delivery", doc.RootElement.GetProperty("incident").GetProperty("name").GetString());
            }
            T.Equal(NvStatus.Unsupported, StatusOf(() => rec.IncidentEdit($$"""{"op":"split","key":"{{incident}}","at":1}""")));

            rec.CameraGroupsPut($$"""{"groups":[{"name":"Group 1","cameras":{{cams}}}]}""");
            T.Check(rec.CameraGroupsGet().Contains("Group 1", StringComparison.Ordinal));
            T.Check(events.Contains(new RecordingsEvent.CameraGroupsChanged(1)));

            // A thumbnail: pending, then ready after its event, then the app's image.
            var key = new ThumbnailKey(new RecordingCamera(Device, 0), "sub", day + 28800, day + 28815);
            T.Check(rec.ThumbnailGet(key.Json) is null);
            fake.CompleteRecordingWork();
            T.Check(events.Contains(new RecordingsEvent.Thumbnail(key, "ready", null)));
            RecordedThumbnail t = T.NotNull(rec.ThumbnailGet(key.Json));
            T.Equal(NvThumbKind.H264Keyframe, t.Kind);
            var image = new RecordedThumbnail(NvThumbKind.Image, 64, 36, 0, [0xff, 0xd8, 0xff, 0xd9]);
            rec.ThumbnailPut(key.Json, image);
            RecordedThumbnail back = T.NotNull(rec.ThumbnailGet(key.Json));
            T.Equal(NvThumbKind.Image, back.Kind);
            T.Check(back.Data.SequenceEqual(image.Data));
            rec.ThumbnailCancel(null);

            // A download to the end, a lock that finds it, and the Recycle Bin.
            string id;
            using (JsonDocument q = JsonDocument.Parse(rec.DownloadIncident($$"""{"incident":"{{incident}}"}""")))
            {
                id = q.RootElement.GetProperty("download").GetString()!;
                T.Check(!q.RootElement.GetProperty("existing").GetBoolean());
            }
            T.Check(rec.DownloadIncident($$"""{"incident":"{{incident}}"}""").Contains("\"existing\":true", StringComparison.Ordinal));
            fake.CompleteRecordingWork();
            T.Equal("queued,running,done", string.Join(",", events.OfType<RecordingsEvent.Download>().Select(d => d.Progress.State)));
            T.Check(events.OfType<RecordingsEvent.Download>().Last().Progress.Cameras[0].Output is not null);
            T.Equal(id, T.NotNull(DownloadProgress.ParseList(rec.DownloadsGet()))[0].Download);
            string locked = rec.IncidentLock(incident, true);
            T.Check(locked.Contains("\"download_status\":\"existing\"", StringComparison.Ordinal) && locked.Contains(id, StringComparison.Ordinal));
            T.Equal(NvStatus.UnknownId, StatusOf(() => rec.DownloadPause("no-such-download", null, true)));
            string folder;
            using (JsonDocument trashed = JsonDocument.Parse(rec.DownloadTrash(id)))
            {
                folder = trashed.RootElement.GetProperty("folder").GetString()!;
            }
            T.Check(events.Contains(new RecordingsEvent.DownloadRemoved(id, true, folder)));
            T.Equal(NvStatus.UnknownId, StatusOf(() => rec.DownloadForget(id, false)));
            T.Check(!rec.IncidentLock(incident, false).Contains("download", StringComparison.Ordinal));
        });
    }
}
