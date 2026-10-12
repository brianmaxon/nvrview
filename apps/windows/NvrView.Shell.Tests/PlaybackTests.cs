using System.Text;
using System.Text.Json;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// Recorded playback (minor 6): the events of docs/FFI-RECORDINGS.md
/// section 6 as the core writes them, the request builders, and
/// FakePlayback from open to close. Synthetic device keys only.
/// </summary>
internal static class PlaybackTests
{
    private const string Device = "d-3f9a0c1b7e2d4a65";

    private static PlaybackEvent? Parse(string json) => PlaybackEvent.Parse(Encoding.UTF8.GetBytes(json));

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
        T.Run("playback: the four events", () =>
        {
            T.Equal(new PlaybackEvent.State(3, NvPlaybackState.Playing, 1791618127000, 1.0),
                Parse("""{"type":"playback_state","playback":3,"state":"playing","position_ms":1791618127000,"rate":1.0}"""));
            T.Equal(new PlaybackEvent.Camera(3, 11, "buffering", null),
                Parse("""{"type":"playback_camera","playback":3,"tile":11,"state":"buffering"}"""));
            T.Equal(new PlaybackEvent.Camera(3, 12, "failed", "start_rejected"),
                Parse("""{"type":"playback_camera","playback":3,"tile":12,"state":"failed","reason":"start_rejected"}"""));
            T.Equal(new PlaybackEvent.Seek(3, 1791618130000, 4, "active"),
                Parse("""{"type":"playback_seek","playback":3,"position_ms":1791618130000,"generation":4,"state":"active","future":1}"""));
            T.Equal(new PlaybackEvent.Closed(3, "core_shutdown"),
                Parse("""{"type":"playback_closed","playback":3,"reason":"core_shutdown"}"""));
            T.Check(Parse("""{"type":"playback_state","playback":3,"state":"warp","position_ms":1}""") is null);
            T.Check(Parse("""{"type":"playback_seek","playback":3,"state":"active"}""") is null);
            T.Check(Parse("""{"type":"playback_camera","state":"playing"}""") is null);
            T.Check(Parse("""{"type":"tile_closed","tile":1,"reason":"closed"}""") is null);
            T.Check(Parse("{") is null);
        });

        T.Run("playback: request builders", () =>
        {
            var cam = new PlaybackCamera(new RecordingCamera(Device, 3), OffsetMs: -200);
            T.Equal($$"""{"camera":{"device":"{{Device}}","channel":3,"lens":0},"tier":"sub","offset_ms":-200}""", cam.Json);
            using JsonDocument doc = JsonDocument.Parse(PlaybackRequest.Range(1791618120000, 1791618146000, [cam], 1791618125000, 0.5));
            T.Equal(1791618125000, doc.RootElement.GetProperty("position_ms").GetInt64());
            T.Equal(0.5, doc.RootElement.GetProperty("rate").GetDouble());
            T.Equal("wait", doc.RootElement.GetProperty("sync").GetString());
            T.Equal(1, doc.RootElement.GetProperty("cameras").GetArrayLength());
            using JsonDocument incident = JsonDocument.Parse(PlaybackRequest.Incident("i-\"1\""));
            T.Equal("i-\"1\"", incident.RootElement.GetProperty("incident").GetString());
            T.Equal("sub", incident.RootElement.GetProperty("tier").GetString());
        });

        T.Run("playback: FakePlayback from open to close", () =>
        {
            var events = new List<string>();
            void Sink(string e)
            {
                lock (events)
                {
                    events.Add(e);
                }
            }
            List<PlaybackEvent> Take()
            {
                lock (events)
                {
                    var parsed = events.Select(e => PlaybackEvent.Parse(Encoding.UTF8.GetBytes(e))).OfType<PlaybackEvent>().ToList();
                    events.Clear();
                    return parsed;
                }
            }
            bool Closed(ulong tile)
            {
                lock (events)
                {
                    return events.Any(e => e.Contains("\"tile_closed\"", StringComparison.Ordinal) &&
                        e.Contains($"\"tile\":{tile},", StringComparison.Ordinal));
                }
            }
            long now = 1_000;
            var rec = new FakeRecordings(Sink) { RecordingWorkIsManual = true };
            IPlayback pb = new FakePlayback(Sink, rec, () => now) { RecordingWorkIsManual = true };
            var fake = (FakePlayback)pb;
            PlaybackCamera[] cams = [new(new RecordingCamera(Device, 0)), new(new RecordingCamera(Device, 1))];
            long start = 1_791_590_400_000 + 10 * 3_600_000, p0 = start + 30_000;
            string request = PlaybackRequest.Range(start, start + 600_000, cams, p0);

            T.Equal(NvStatus.NotConfigured, StatusOf(() => pb.PlaybackOpen(request)));
            rec.RecordingsConfigure("{}");
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => pb.PlaybackOpen(request)));
            rec.RecordingsEnable(Device, """{"channels":"all"}""");
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => pb.PlaybackOpen(PlaybackRequest.Range(start, start + 1000, cams, rate: 3))));
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => pb.PlaybackOpen(PlaybackRequest.Range(start, start + 1000, [.. cams, .. cams]))));
            T.Equal(NvStatus.Limit, StatusOf(() => pb.PlaybackOpen(PlaybackRequest.Range(start, start + 25 * 3_600_000, cams))));
            T.Equal(NvStatus.UnknownId, StatusOf(() => pb.PlaybackOpen(PlaybackRequest.Incident("i-none"))));

            OpenedPlayback opened = pb.PlaybackOpen(request);
            T.Equal(2, opened.Tiles.Count);
            T.Check(!opened.Tiles.Contains(opened.Playback));
            T.Equal(0, Take().Count);
            fake.CompletePlaybackWork();
            List<PlaybackEvent> first = Take();
            T.Check(first.Contains(new PlaybackEvent.Seek(opened.Playback, p0, 1, "active")));
            T.Check(first.Contains(new PlaybackEvent.State(opened.Playback, NvPlaybackState.Paused, p0, 1)));
            T.Check(first.Contains(new PlaybackEvent.Camera(opened.Playback, opened.Tiles[1], "playing", null)));
            T.Equal((p0, NvPlaybackState.Paused), pb.PlaybackPosition(opened.Playback));

            pb.PlaybackPlay(opened.Playback);
            now += 5000;
            T.Equal((p0 + 5000, NvPlaybackState.Playing), pb.PlaybackPosition(opened.Playback));
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => pb.PlaybackSetRate(opened.Playback, 3)));
            pb.PlaybackSetRate(opened.Playback, 4);
            now += 2000;
            T.Equal(p0 + 13_000, pb.PlaybackPosition(opened.Playback).PositionMs);
            fake.CompletePlaybackWork();
            T.Check(Take().Contains(new PlaybackEvent.State(opened.Playback, NvPlaybackState.Playing, p0 + 5000, 4)));

            T.Equal(NvStatus.InvalidArgument, StatusOf(() => pb.PlaybackSeek(opened.Playback, p0, (NvSeekMode)9)));
            pb.PlaybackSeek(opened.Playback, start + 120_000, NvSeekMode.Exact);
            fake.CompletePlaybackWork();
            T.Check(Take().Contains(new PlaybackEvent.Seek(opened.Playback, start + 120_000, 3, "active")));
            pb.PlaybackSetOffset(opened.Playback, opened.Tiles[1], 500);
            T.Equal(NvStatus.UnknownId, StatusOf(() => pb.PlaybackSetOffset(opened.Playback, 999, 0)));
            using (JsonDocument status = JsonDocument.Parse(pb.PlaybackStatus(opened.Playback)))
            {
                T.Equal(4.0, status.RootElement.GetProperty("rate").GetDouble());
                T.Equal(500, status.RootElement.GetProperty("cameras")[1].GetProperty("offset_ms").GetInt64());
            }

            pb.PlaybackPause(opened.Playback);
            now += 1000;
            T.Equal(start + 120_000, pb.PlaybackPosition(opened.Playback).PositionMs);
            pb.PlaybackPlay(opened.Playback);
            now += 600_000;
            T.Equal((start + 600_000, NvPlaybackState.Ended), pb.PlaybackPosition(opened.Playback));

            pb.PlaybackRemoveCamera(opened.Playback, opened.Tiles[1]);
            T.Check(Closed(opened.Tiles[1]));
            T.Equal(NvStatus.UnknownId, StatusOf(() => pb.PlaybackRemoveCamera(opened.Playback, opened.Tiles[1])));
            ulong added = pb.PlaybackAddCamera(opened.Playback, cams[1].Json);
            T.Check(!opened.Tiles.Contains(added));
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => pb.PlaybackAddCamera(opened.Playback, cams[1].Json)));

            _ = Take();
            pb.PlaybackClose(opened.Playback);
            T.Check(Closed(opened.Tiles[0]) && Closed(added));
            T.Equal(new PlaybackEvent.Closed(opened.Playback, "closed"), Take().LastOrDefault());
            T.Equal(NvStatus.UnknownId, StatusOf(() => pb.PlaybackPosition(opened.Playback)));
            T.Equal(NvStatus.UnknownId, StatusOf(() => pb.PlaybackClose(opened.Playback)));
        });

        T.Run("playback: FakePlayback opens an incident", () =>
        {
            var events = new List<string>();
            var rec = new FakeRecordings(e => events.Add(e)) { RecordingWorkIsManual = true };
            var pb = new FakePlayback(e => events.Add(e), rec) { RecordingWorkIsManual = true };
            long day = 1_791_590_400;
            rec.RecordingsConfigure("{}");
            rec.RecordingsEnable(Device, """{"channels":[{"channel":0}]}""");
            rec.RecordingsWant($$"""{"cameras":[{"device":"{{Device}}","channel":0}],"days":[{{day}}]}""");
            string incident;
            using (JsonDocument doc = JsonDocument.Parse(rec.IncidentsGet($$"""{"start":{{day}},"end":{{day + 86399}}}""")))
            {
                incident = doc.RootElement.GetProperty("incidents")[0].GetProperty("key").GetString()!;
            }
            OpenedPlayback opened = pb.PlaybackOpen(PlaybackRequest.Incident(incident));
            T.Equal(1, opened.Tiles.Count);
            using (JsonDocument status = JsonDocument.Parse(pb.PlaybackStatus(opened.Playback)))
            {
                long s = status.RootElement.GetProperty("start_ms").GetInt64();
                long e = status.RootElement.GetProperty("end_ms").GetInt64();
                T.Check(e > s && s % 1000 == 0);
            }
            pb.PlaybackClose(opened.Playback);
        });
    }
}
