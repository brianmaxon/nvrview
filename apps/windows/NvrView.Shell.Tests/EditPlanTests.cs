// The follow cut and grids (EditPlan.cs, Mp4Index.cs, EditPlayer.cs); the same vectors as the
// macOS app's EditPlanTests, then the player's flow against the fakes.

using System.Buffers.Binary;
using System.Text;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

internal static class EditPlanTests
{
    private const string D = "d-3f9a0c1b7e2d4a65";

    private static string Cam(int channel) => $$"""{"device":"d-example","channel":{{channel}},"lens":0}""";

    // Section 7's example shape: two shots of a follow cut.
    private static readonly string s_follow = $$"""
        {"version": 1, "kind": "follow", "download": "dl-1", "start_ms": 100000, "end_ms": 126000,
         "suggested_file": "<folder>/follow.mp4", "copy_possible": true,
         "sources": [
          {"source": 0, "camera": {{Cam(3)}}, "name": "Camera 1", "file": "/x/1.mp4", "file_start_ms": 99400, "offset_ms": 0,
           "codec": "h264", "width": 2560, "height": 1440},
          {"source": 1, "camera": {{Cam(5)}}, "name": "Camera 2", "file": "/x/2.mp4", "file_start_ms": 106900, "offset_ms": 0,
           "codec": "h264", "width": 2560, "height": 1440}],
         "segments": [{"start_ms": 100000, "end_ms": 111000, "source": 0, "source_in_ms": 600},
                      {"start_ms": 111000, "end_ms": 120000, "source": 1, "source_in_ms": 4100},
                      {"start_ms": 121000, "end_ms": 126000, "source": 1, "source_in_ms": 14100}],
         "labels": [{"source": 0, "name": "Camera 1", "time": true}]}
        """;

    private static readonly string s_grid = $$"""
        {"version": 1, "kind": "grid", "download": "dl-1", "layout": "2up", "start_ms": 100000, "end_ms": 130000,
         "suggested_file": "x", "copy_possible": false,
         "sources": [
          {"source": 0, "camera": {{Cam(3)}}, "name": "Camera 1", "file": "/x/1.mp4", "file_start_ms": 99000, "offset_ms": 0,
           "codec": "h264", "width": 1920, "height": 1080},
          {"source": 1, "camera": {{Cam(5)}}, "name": "Camera 2", "file": "/x/2.mp4", "file_start_ms": 105000, "offset_ms": -400,
           "codec": "h264", "width": 1920, "height": 1080}],
         "canvas": {"width": 1920, "height": 1080},
         "cells": [{"rect": {"x": 0, "y": 0, "width": 960, "height": 1080}, "source": 0, "fit": "fit", "present": [[100000, 130000]]},
                   {"rect": {"x": 960, "y": 0, "width": 960, "height": 1080}, "source": 1, "fit": "fill", "present": [[105000, 125000]]}]}
        """;

    public static void Run()
    {
        T.Run("edits: parses both kinds", () =>
        {
            EditPlan f = T.NotNull(EditPlan.Parse(s_follow));
            T.Check(f.Kind == EditKind.Follow && f.Sources.Count == 2 && f.Segments.Count == 3 && f.CopyPossible);
            T.Equal(new RecordingCamera("d-example", 5), f.Sources[1].Camera);
            T.Check(f.Labels[0].Time);
            EditPlan g = T.NotNull(EditPlan.Parse(s_grid));
            T.Check(g.Kind == EditKind.Grid && g.Cells.Count == 2 && g.Canvas == new EditCanvas(1920, 1080));
            T.Check(g.Sources[1].OffsetMs == -400 && g.Cells[1].Fit == "fill");
            // A plan naming a source it does not list, or not a plan at all.
            T.Check(EditPlan.Parse(s_follow.Replace("\"source\": 1, \"source_in_ms\": 4100", "\"source\": 7, \"source_in_ms\": 4100")) is null);
            T.Check(EditPlan.Parse("{}") is null && EditPlan.Parse("not json") is null);
        });

        T.Run("edits: the follow cut maps time to shot and file position", () =>
        {
            EditPlan p = T.NotNull(EditPlan.Parse(s_follow));
            T.Equal<EditShot?>(new EditShot(0, 0, 600), EditTimeline.Shot(p, 100000));
            T.Equal<EditShot?>(new EditShot(0, 0, 11599), EditTimeline.Shot(p, 110999));
            // The cut instant shows the incoming shot.
            T.Equal<EditShot?>(new EditShot(1, 1, 4100), EditTimeline.Shot(p, 111000));
            // A gap is black; the plan's last instant shows the last picture.
            T.Check(EditTimeline.Shot(p, 120500) is null);
            T.Equal(19100L, EditTimeline.Shot(p, 126000)?.FilePositionMs);
            T.Check(EditTimeline.Shot(p, 99999) is null && EditTimeline.Shot(p, 126001) is null);
        });

        T.Run("edits: cuts are source changes and gaps", () =>
        {
            EditPlan p = T.NotNull(EditPlan.Parse(s_follow));
            T.Check(EditTimeline.Cuts(p).SequenceEqual([111000, 121000]));
            EditPlan same = new()
            {
                Kind = EditKind.Follow,
                StartMs = 0,
                EndMs = 30,
                Sources = p.Sources,
                Segments = [new EditSegment(0, 10, 0, 0), new EditSegment(10, 20, 0, 10), new EditSegment(20, 30, 1, 0)],
            };
            T.Check(EditTimeline.Cuts(same).SequenceEqual([20]));
            T.Check(EditTimeline.Cuts(T.NotNull(EditPlan.Parse(s_grid))).Count == 0);
            // The next camera and where it starts (the renderer decodes it early).
            T.Equal(111000L, EditTimeline.NextShot(p, 105000)?.At);
            T.Equal(new EditShot(1, 1, 4100), EditTimeline.NextShot(p, 105000)?.Shot);
            T.Check(EditTimeline.NextShot(p, 112000) is null);
            T.Equal(0, EditTimeline.NextShot(p, 99000)?.Shot.Source);
        });

        T.Run("edits: grid cells are present where their files have video", () =>
        {
            EditPlan p = T.NotNull(EditPlan.Parse(s_grid));
            T.Equal<long?>(1000, EditTimeline.CellPosition(p, 0, 100000));
            // The offset moves the camera's clock: -400 ms means its file is read 400 ms later.
            T.Equal<long?>(5400, EditTimeline.CellPosition(p, 1, 110000));
            T.Check(EditTimeline.CellPosition(p, 1, 104999) is null);
            T.Check(EditTimeline.CellPosition(p, 1, 125000) is null);
            T.Equal<long?>(31000, EditTimeline.CellPosition(p, 0, 130000));
            T.Check(EditTimeline.CellPosition(p, 2, 110000) is null);
        });

        T.Run("edits: sound follows the picture", () =>
        {
            EditPlan f = T.NotNull(EditPlan.Parse(s_follow));
            T.Equal<int?>(0, EditTimeline.AudibleSource(f, 105000, 1, false));
            T.Equal<int?>(1, EditTimeline.AudibleSource(f, 111000, 1, false));
            T.Check(EditTimeline.AudibleSource(f, 120500, 1, false) is null);
            T.Check(EditTimeline.AudibleSource(f, 105000, 1, true) is null);
            T.Check(EditTimeline.AudibleSource(f, 105000, 2, false) is null);
            T.Check(EditTimeline.AudibleSource(f, 105000, 0.5, false) is null);
            EditPlan g = T.NotNull(EditPlan.Parse(s_grid));
            T.Equal<int?>(0, EditTimeline.AudibleSource(g, 110000, 1, false));
            T.Equal<int?>(1, EditTimeline.AudibleSource(g, 110000, 1, false, gridCell: 1));
            T.Check(EditTimeline.AudibleSource(g, 101000, 1, false, gridCell: 1) is null);
        });

        T.Run("edits: the canvas is letterboxed and the cells scale with it", () =>
        {
            EditPlan g = T.NotNull(EditPlan.Parse(s_grid));
            // A square view: the 16:9 canvas sits in the middle, its cells scaled.
            var bounds = new Rect(0, 0, 960, 960);
            T.Equal(new Rect(0, 210, 960, 540), EditComposition.Frame(g, bounds));
            T.Check(EditComposition.CellRects(g, bounds).SequenceEqual([new Rect(0, 210, 480, 540), new Rect(480, 210, 480, 540)]));
            List<EditLayer> layers = EditComposition.Layers(g, 110000, bounds);
            T.Check(layers.Select(l => l.Source).SequenceEqual([0, 1]) && layers.Select(l => l.Fill).SequenceEqual([false, true]));
            T.Check(layers.Select(l => l.Cell).SequenceEqual([0, 1]));
            T.Check(EditComposition.Layers(g, 101000, bounds).Select(l => l.Source).SequenceEqual([0]));
            EditPlan f = T.NotNull(EditPlan.Parse(s_follow));
            T.Check(EditComposition.Layers(f, 112000, bounds).SequenceEqual([new EditLayer(1, 5100, bounds, false)]));
            // Fitting and filling a 640 by 360 picture.
            PictureFit fit = PictureFit.Of(640, 360, new Rect(0, 0, 320, 320), fill: false);
            T.Equal(new Rect(0, 70, 320, 180), fit.Destination);
            T.Equal(new Rect(0, 0, 640, 360), fit.Source);
            PictureFit fill = PictureFit.Of(640, 360, new Rect(0, 0, 320, 320), fill: true);
            T.Equal(new Rect(0, 0, 320, 320), fill.Destination);
            T.Equal(new Rect(140, 0, 360, 360), fill.Source);
        });

        T.Run("edits: the clock runs, pauses, seeks, and stops at the end", () =>
        {
            var c = new EditClock(1000, 11000);
            T.Check(c.Position(50) == 1000 && c.State(50) == NvPlaybackState.Paused);
            c.Play(100);
            T.Check(c.Position(102.5) == 3500 && c.State(102.5) == NvPlaybackState.Playing);
            c.SetRate(4, 102.5);
            T.Equal(5500L, c.Position(103));
            c.Pause(103);
            T.Equal(5500L, c.Position(200));
            c.Seek(10500, 200);
            c.SetRate(1, 200);
            c.Play(200);
            T.Check(c.Position(201) == 11000 && c.State(201) == NvPlaybackState.Ended);
            // Play at the end starts again.
            c.Play(201);
            T.Check(c.Position(201) == 1000 && c.State(201) == NvPlaybackState.Playing);
            c.Seek(0, 202);
            T.Equal(1000L, c.Position(202));
            T.Check(c.Transport(202).IsPlaying);
        });

        T.Run("edits: an MP4's keyframes, size, and codec from its sample tables", () =>
        {
            // 60 pictures at 10 per second, a keyframe every 20.
            byte[] plain = Mp4(timescale: 10, deltas: [(60, 1)], sync: [1, 21, 41], offsets: null, edit: null);
            Mp4VideoInfo info = T.NotNull(Mp4Index.Read(new MemoryStream(plain)));
            T.Check(info.KeyframesMs.SequenceEqual([0, 2000, 4000]));
            T.Check(info.Width == 640 && info.Height == 360 && info.Codec == "h264" && info.DurationMs == 6000, $"{info}");
            T.Equal(2000L, info.KeyframeAtOrBefore(3999));
            T.Equal(0L, info.KeyframeAtOrBefore(-5));
            T.Equal(4000L, info.KeyframeAtOrBefore(99999));
            // Frame reordering: presentation is decode time plus the offset, less the edit's start.
            byte[] reordered = Mp4(timescale: 10, deltas: [(60, 1)], sync: [1, 21], offsets: [(60, 2)], edit: (0, 2, 0));
            T.Check(T.NotNull(Mp4Index.Read(new MemoryStream(reordered))).KeyframesMs.SequenceEqual([0, 2000]), "reordered");
            // A leading empty edit delays the track by its length (movie timescale 1000).
            byte[] delayed = Mp4(timescale: 10, deltas: [(60, 1)], sync: [1, 21], offsets: null, edit: (500, 0, 0));
            T.Check(T.NotNull(Mp4Index.Read(new MemoryStream(delayed))).KeyframesMs.SequenceEqual([500, 2500]), "delayed");
            // No stss: every picture is a sync sample. Variable deltas.
            byte[] every = Mp4(timescale: 10, deltas: [(2, 1), (2, 3)], sync: null, offsets: null, edit: null);
            T.Check(T.NotNull(Mp4Index.Read(new MemoryStream(every))).KeyframesMs.SequenceEqual([0, 100, 200, 500]), "every");
            T.Check(Mp4Index.Read(new MemoryStream([1, 2, 3, 4, 5, 6, 7, 8, 9])) is null);
            _ = Mp4Index.Read(new MemoryStream(plain.AsSpan(0, plain.Length - 20).ToArray()));
            T.Check(Mp4Index.Read(Path.Combine(Path.GetTempPath(), "nvrview-no-such-file.mp4")) is null);
        });

        T.Run("edits: the view menu's requests", () =>
        {
            T.Equal("Follow Cut", PlayerViews.Title(PlayerView.Follow));
            T.Equal("4-Up", PlayerViews.Title(PlayerView.Grid4));
            T.Check(PlayerViews.Request(PlayerView.Cameras, "d") is null);
            T.Equal("""{"kind":"follow","download":"d"}""", PlayerViews.Request(PlayerView.Follow, "d"));
            T.Equal("""{"kind":"grid","download":"d","layout":"2up","canvas":{"width":1920,"height":1080}}""", PlayerViews.Request(PlayerView.Grid2, "d"));
        });

        T.Run("edits: the follow cut and grids over a downloaded incident, against the fakes", Flow);
    }

    // A box of the MP4 format.
    private static byte[] Box(string type, params byte[][] parts)
    {
        int size = 8 + parts.Sum(p => p.Length);
        byte[] box = new byte[size];
        BinaryPrimitives.WriteUInt32BigEndian(box, (uint)size);
        Encoding.ASCII.GetBytes(type).CopyTo(box, 4);
        int at = 8;
        foreach (byte[] p in parts)
        {
            p.CopyTo(box, at);
            at += p.Length;
        }
        return box;
    }

    private static byte[] U32s(params uint[] values)
    {
        byte[] b = new byte[values.Length * 4];
        for (int i = 0; i < values.Length; i++)
        {
            BinaryPrimitives.WriteUInt32BigEndian(b.AsSpan(i * 4), values[i]);
        }
        return b;
    }

    // A file with one video track: 640 by 360 H.264 pictures, movie timescale 1000.
    // edit: (empty edit length in movie ticks, first real edit's media time, 0); null: no edit list.
    private static byte[] Mp4(uint timescale, (uint Count, uint Delta)[] deltas, uint[]? sync, (uint Count, int Offset)[]? offsets, (uint Empty, uint Media, uint Unused)? edit)
    {
        uint total = (uint)deltas.Sum(d => (int)(d.Count * d.Delta));
        byte[] entry = new byte[86];
        BinaryPrimitives.WriteUInt32BigEndian(entry, 86);
        Encoding.ASCII.GetBytes("avc1").CopyTo(entry, 4);
        BinaryPrimitives.WriteUInt16BigEndian(entry.AsSpan(32), 640);
        BinaryPrimitives.WriteUInt16BigEndian(entry.AsSpan(34), 360);
        var stbl = new List<byte[]>
        {
            Box("stsd", U32s(0, 1), entry),
            Box("stts", U32s(0, (uint)deltas.Length), U32s([.. deltas.SelectMany(d => new[] { d.Count, d.Delta })])),
        };
        if (sync is not null)
        {
            stbl.Add(Box("stss", U32s(0, (uint)sync.Length), U32s(sync)));
        }
        if (offsets is not null)
        {
            stbl.Add(Box("ctts", U32s(0, (uint)offsets.Length), U32s([.. offsets.SelectMany(o => new[] { o.Count, unchecked((uint)o.Offset) })])));
        }
        byte[] hdlr = Box("hdlr", U32s(0, 0), Encoding.ASCII.GetBytes("vide"), new byte[12]);
        byte[] mdhd = Box("mdhd", U32s(0, 0, 0, timescale, total));
        byte[] mdia = Box("mdia", mdhd, hdlr, Box("minf", Box("stbl", [.. stbl])));
        var trak = new List<byte[]> { Box("tkhd", new byte[84]) };
        if (edit is { } e)
        {
            var entries = new List<uint>();
            uint count = 1;
            if (e.Empty > 0)
            {
                entries.AddRange([e.Empty, 0xFFFFFFFF, 0x00010000]);
                count = 2;
            }
            entries.AddRange([total * 100, e.Media, 0x00010000]);
            trak.Add(Box("edts", Box("elst", U32s(0, count), U32s([.. entries]))));
        }
        trak.Add(mdia);
        byte[] moov = Box("moov", Box("mvhd", U32s(0, 0, 0, 1000, 0)), Box("trak", [.. trak]));
        return [.. Box("ftyp", Encoding.ASCII.GetBytes("isom"), new byte[8]), .. moov];
    }

    // ---- the player against the fakes ----

    private sealed class Files(EditPlan plan) : IEditFiles
    {
        public EditPlan Plan { get; } = plan;

        public IReadOnlyDictionary<int, string> Failures { get; } = new Dictionary<int, string>();

        public bool AnyOpen => true;

        public List<(long T, bool Keyframes)> Requests { get; } = [];

        public bool Disposed { get; private set; }

        public string? FileOf(int source) => Plan.SourceOf(source)?.File;

        public void Request(long t, bool keyframesOnly) => Requests.Add((t, keyframesOnly));

        public long? ShownPositionMs(int source) => null;

        public void Dispose() => Disposed = true;
    }

    private sealed class Factory : IEditFilesFactory
    {
        public List<Files> Opened { get; } = [];

        public Task<IEditFiles> OpenAsync(EditPlan plan, CancellationToken cancel)
        {
            var f = new Files(plan);
            Opened.Add(f);
            return Task.FromResult<IEditFiles>(f);
        }
    }

    private sealed class Audio : IEditAudio
    {
        public int? Active { get; private set; }

        public List<(int? Source, long? Position, bool Playing)> Updates { get; } = [];

        public void Update(int? source, string? file, long? filePositionMs, bool playing, float volume)
        {
            Active = playing ? source : null;
            Updates.Add((source, filePositionMs, playing));
        }

        public void Close() => Active = null;

        public void Dispose() => Close();
    }

    private static void Flow()
    {
        var queue = new Queue<Action>();
        long now = 1_000;
        RecordingsService? built = null;
        void Sink(string json) => built!.Route(Encoding.UTF8.GetBytes(json));
        var fake = new FakeRecordings(Sink) { RecordingWorkIsManual = true };
        var playback = new FakePlayback(Sink, fake, () => now) { RecordingWorkIsManual = true };
        RecordingsService service = built = new RecordingsService(fake, new PlainPlaybackHost(playback))
        {
            Post = queue.Enqueue,
            Now = () => DateTimeOffset.UtcNow,
        };
        service.Start(Path.Combine(Path.GetTempPath(), "nvrview-edit-tests-" + Environment.ProcessId, "Incidents"));
        void Pump()
        {
            for (int i = 0; i < 100 && queue.Count > 0; i++)
            {
                queue.Dequeue()();
            }
        }
        void Work()
        {
            fake.CompleteRecordingWork();
            playback.CompletePlaybackWork();
            Pump();
        }
        RecordingCamera[] cams = [.. Enumerable.Range(0, 3).Select(c => new RecordingCamera(D, c))];
        foreach (RecordingCamera c in cams)
        {
            service.SetEnabled(c, true);
        }
        var files = new Factory();
        var audio = new Audio();
        var b = new RecordingsBrowser(service, files, audio);
        Pump();

        // The vehicle incident of the first camera; the others' vehicle events run along with it.
        Incident incident = b.DayIncidents.First(i => i.Kinds.SequenceEqual(["vehicle"]) && i.Cameras[0].Camera == cams[0]);
        long s = incident.Start * 1000;
        b.Click(incident.Key, extend: false);
        Work();
        T.Check(b.Editable is null, "an incident not downloaded offers no edit");
        b.ShowView(PlayerView.Follow);
        T.Check(b.PlayerView == PlayerView.Cameras && b.Message is not null, "Follow Cut without a download stays on the cameras");
        b.Message = null;

        // Download the three cameras' files.
        string request = $$"""{"incident":"{{incident.Key}}","cameras":[{{string.Join(",", cams.Select(c => $$"""{"camera":{{c.Json}}}"""))}}]}""";
        fake.DownloadIncident(request);
        for (int i = 0; i < 5 && service.FinishedDownload(incident.Key) is null; i++)
        {
            Work();
        }
        T.Check(b.Editable is not null, "a finished download offers the edit (edit-download)");

        // The follow cut: the plan, the cuts, the device playback paused.
        b.ShowView(PlayerView.Follow);
        EditPlayer ep = b.EditPlayer;
        double clock = 500;
        ep.Now = () => clock;
        Work();
        T.Check(b.PlayerView == PlayerView.Follow && ep.Plan is not null && ep.Files is not null, ep.Message ?? b.Message);
        EditPlan plan = ep.Plan!;
        T.Check(ep.Cuts.SequenceEqual([s + 1000, s + 2000]), $"{string.Join(",", ep.Cuts.Select(c => c - s))}");
        T.Check(plan.Segments.Select(g => g.Source).SequenceEqual([0, 1, 2]) && plan.Sources.Count == 3);
        T.Check(!b.Player.Transport.IsPlaying, "the device playback pauses under the edit");
        T.Equal((plan.StartMs, plan.EndMs), b.ScrubRange);

        // The transport on a simulated clock, and the sound following the picture.
        ep.Pause();
        ep.SetRate(1);
        ep.Seek(s + 500);
        ep.Play();
        var heard = new List<int?> { ep.PlayingSoundSource };
        clock += 1;
        ep.Tick();
        heard.Add(ep.PlayingSoundSource);
        clock += 1;
        ep.Tick();
        heard.Add(ep.PlayingSoundSource);
        T.Equal(s + 2500, ep.PositionMs);
        ep.SetRate(4);
        heard.Add(ep.PlayingSoundSource);
        ep.SetRate(1);
        ep.Muted = true;
        heard.Add(ep.PlayingSoundSource);
        ep.Muted = false;
        ep.Pause();
        T.Check(heard.SequenceEqual([0, 1, 2, null, null]), string.Join(",", heard.Select(h => h?.ToString() ?? "-")));
        T.Check(!ep.KeyframesOnly);
        ep.SetRate(2);
        T.Check(ep.KeyframesOnly, "2x decodes keyframes only");
        ep.SetRate(1);
        ep.Seek(s + 100, exact: false);
        T.Check(ep.KeyframesOnly && ep.Scrubbing, "dragging decodes keyframes only");
        ep.Seek(s + 100);
        T.Check(!ep.KeyframesOnly);

        // A lane click inside the range seeks the edit; another time brings the cameras back.
        b.LaneClick(cams[1], (s + 4000) / 1000);
        T.Check(b.PlayerView == PlayerView.Follow && ep.PositionMs == s + 4000 && ep.Transport.IsPlaying, "lane click seeks the edit");
        ep.Pause();

        // The 4-up grid: three cells on the canvas; the sound is the first cell's, or the one clicked.
        b.ShowView(PlayerView.Grid4);
        T.Check(b.PlayerView == PlayerView.Grid4 && ep.Plan?.Kind == EditKind.Grid && ep.Plan.Cells.Count == 3, ep.Message);
        T.Check(ep.Plan!.Layout == "4up" && ep.Plan.Canvas == new EditCanvas(1920, 1080));
        ep.Seek(ep.Plan.StartMs + 6000);
        ep.Play();
        int? first = ep.PlayingSoundSource;
        ep.GridSound = 1;
        int? second = ep.PlayingSoundSource;
        ep.Pause();
        ep.GridSound = 0;
        T.Check(first == 0 && second == 1, $"{first}, {second}");

        // Back to the cameras: the edit closes and the device playback continues from its position.
        long at = ep.PositionMs;
        b.ShowView(PlayerView.Cameras);
        T.Check(b.PlayerView == PlayerView.Cameras && ep.Files is null && files.Opened.All(f => f.Disposed), "the edit closed");
        T.Equal(at, b.Player.PositionMs);
        b.Close();
    }
}
