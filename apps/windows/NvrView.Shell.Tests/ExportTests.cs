// Export (docs/UX.md "Recordings", "Export"): the in and out points, the requests and plans, the output rules (the
// same vectors as the macOS app's ExportTests), the share-safe patch, and the window's export over the fakes with
// writers that write small made-up files.

using System.Buffers.Binary;
using System.Text;
using System.Text.Json;
using NvrView.Tiles;

namespace NvrView.Shell.Tests;

internal static class ExportTests
{
    private const string Device = "d-3f9a0c1b7e2d4a65";
    private const long Lo = 10_000, Hi = 20_000;

    // ---- a small MP4 with the boxes share-safe cares about ----

    private static byte[] Box(string type, params byte[][] parts)
    {
        int size = 8 + parts.Sum(p => p.Length);
        var b = new byte[size];
        BinaryPrimitives.WriteUInt32BigEndian(b, (uint)size);
        Encoding.ASCII.GetBytes(type).CopyTo(b, 4);
        int at = 8;
        foreach (byte[] p in parts)
        {
            p.CopyTo(b, at);
            at += p.Length;
        }
        return b;
    }

    private static byte[] Times(byte version, bool tkhd = false)
    {
        // version and flags, then creation and modification times (all ones), then the rest of the box.
        var body = new List<byte> { version, 0, 0, 0 };
        int timeBytes = version == 1 ? 16 : 8;
        body.AddRange(Enumerable.Repeat((byte)0xAB, timeBytes));
        body.AddRange(Enumerable.Repeat((byte)0x01, tkhd ? 24 : 12));
        return [.. body];
    }

    private static byte[] Hdlr(string kind, string name)
    {
        var body = new List<byte> { 0, 0, 0, 0, 0, 0, 0, 0 };
        body.AddRange(Encoding.ASCII.GetBytes(kind));
        body.AddRange(new byte[12]);
        body.AddRange(Encoding.ASCII.GetBytes(name));
        body.Add(0);
        return Box("hdlr", [.. body]);
    }

    internal static byte[] MiniMp4(bool withMetadata = true)
    {
        byte[] track = Box("trak", Box("tkhd", Times(0, tkhd: true)), Box("mdia", Box("mdhd", Times(1)), Hdlr("vide", "Media Foundation Video Handler  ")));
        byte[] soundTrack = Box("trak", Box("mdia", Box("mdhd", Times(0)), Hdlr("soun", "Core Media Audio   ")));
        byte[] user = withMetadata ? Box("udta", Box("meta", Encoding.ASCII.GetBytes("Lavf58.76.100 encoder"))) : [];
        return [.. Box("ftyp", Encoding.ASCII.GetBytes("isom")), .. Box("moov", Box("mvhd", Times(0)), track, soundTrack, user), .. Box("mdat", [1, 2, 3, 4])];
    }

    // ---- the window's export over the fakes ----

    private sealed class Writers : IExportWriters
    {
        public readonly List<(TrimOutput Output, bool Exact, string Partial)> Trims = [];
        public readonly List<(string File, long Ms, string Path, PictureFormat Format)> FileStills = [];
        public readonly List<(ulong Tile, string Path)> TileStills = [];
        public bool CancelMidway;
        public bool FailTrim;

        public bool CanExportEdit => false;

        public ExportOutcome Trim(TrimOutput output, bool exact, string partialPath, ExportControl control)
        {
            Trims.Add((output, exact, partialPath));
            File.WriteAllBytes(partialPath, MiniMp4());
            if (FailTrim)
            {
                return ExportOutcome.Failed(partialPath, "The cut failed");
            }
            if (CancelMidway)
            {
                control.Cancel();
                return ExportOutcome.Canceled(partialPath);
            }
            return new ExportOutcome(ExportResult.Done, partialPath, null, 640, 360, 25);
        }

        public ExportOutcome StillFromFile(string file, long sourceMs, bool exact, PictureFormat format, string path)
        {
            FileStills.Add((file, sourceMs, path, format));
            File.WriteAllBytes(path, [1, 2, 3]);
            return new ExportOutcome(ExportResult.Done, path, null, 640, 360, 0, 3);
        }

        public ExportOutcome StillFromTile(ulong tile, PictureFormat format, string path)
        {
            TileStills.Add((tile, path));
            File.WriteAllBytes(path, [1, 2, 3]);
            return new ExportOutcome(ExportResult.Done, path, null, 640, 360, 0, 3);
        }

        public ExportOutcome ExportEdit(string planJson, ExportCodec codec, string partialPath, ExportControl control) =>
            ExportOutcome.Failed(partialPath, "no renderer");
    }

    private sealed class Dialogs(string folder) : IExportDialogs
    {
        public readonly List<string> Asked = [];
        public bool Cancel;

        public Task<string?> SaveFile(string suggestedName, string? start, string extension)
        {
            Asked.Add($"save {suggestedName} from {(start is null ? "-" : Path.GetFileName(start))}");
            return Task.FromResult<string?>(Cancel ? null : Path.Combine(folder, suggestedName));
        }

        public Task<string?> ChooseFolder(string message, string? start)
        {
            Asked.Add($"folder from {(start is null ? "-" : Path.GetFileName(start))}");
            return Task.FromResult<string?>(Cancel ? null : folder);
        }
    }

    private sealed class Rig
    {
        public readonly Queue<Action> Queue = new();
        public readonly FakeRecordings Recordings;
        public readonly FakePlayback Playback;
        public readonly RecordingsService Service;
        public readonly RecordingsBrowser Browser;
        public readonly Writers Writers = new();
        public readonly Dialogs Dialogs;
        public readonly string Folder;
        public readonly RecordingCamera[] Cameras;

        public Rig(ExportSettings? settings = null)
        {
            Folder = Path.Combine(Path.GetTempPath(), "nvrview-export-tests-" + Guid.NewGuid().ToString("N")[..8]);
            Directory.CreateDirectory(Folder);
            Dialogs = new Dialogs(Folder);
            Recordings = new FakeRecordings(Sink) { RecordingWorkIsManual = true };
            Playback = new FakePlayback(Sink, Recordings, () => 1_000) { RecordingWorkIsManual = true };
            Service = new RecordingsService(Recordings, new PlainPlaybackHost(Playback))
            {
                Post = a => a(),
                Now = () => DateTimeOffset.UtcNow,
            };
            Service.Start(Path.Combine(Folder, "Incidents"));
            Cameras = [.. Enumerable.Range(0, 3).Select(c => new RecordingCamera(Device, c))];
            foreach (RecordingCamera c in Cameras)
            {
                Service.SetEnabled(c, true);
            }
            Browser = new RecordingsBrowser(Service, settings);
            Browser.Export.Writers = Writers;
            Browser.Export.Dialogs = Dialogs;
            Pump();
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

        /// <summary>Plays the first incident and downloads it (done).</summary>
        public Incident PlayAndDownload()
        {
            Incident first = Browser.DayIncidents.OrderBy(i => i.Start).First();
            Browser.Click(first.Key, extend: false);
            Work();
            Browser.Download(first.Key);
            Work();
            return first;
        }

        public (long Lo, long Hi) Scrub => Browser.ScrubRange!.Value;

        public void Dispose()
        {
            Browser.Close();
            try
            {
                Directory.Delete(Folder, recursive: true);
            }
            catch (IOException)
            {
            }
        }
    }

    public static void Run()
    {
        T.Run("export: points are set, clamped, and never cross", () =>
        {
            InOutPoints p = default;
            T.Check(p.IsEmpty && p.Window(Lo, Hi) == (10_000, 20_000));
            p = p.SetIn(12_000, Lo, Hi).SetOut(15_000, Lo, Hi);
            T.Check(p.Window(Lo, Hi) == (12_000, 15_000));
            p = p.SetIn(99_000, Lo, Hi); // clamped to the end: the out point goes
            T.Check(p.InMs == 20_000 && p.OutMs is null && p.Window(Lo, Hi) is null);
            p = p.SetOut(11_000, Lo, Hi); // before the in point: the in point goes
            T.Check(p.InMs is null && p.OutMs == 11_000 && p.Window(Lo, Hi) == (10_000, 11_000));
            p = p.SetIn(5_000, Lo, Hi);
            T.Equal(10_000L, p.InMs);
            T.Check(default(InOutPoints).IsEmpty);
        });

        T.Run("export: points outside a new range are dropped", () =>
        {
            var p = new InOutPoints(12_000, 18_000);
            T.Equal(p, p.Kept(11_000, 19_000));
            T.Equal(new InOutPoints(null, 18_000), p.Kept(13_000, 19_000));
            T.Check(p.Kept(0, 5_000).IsEmpty);
            T.Equal("start to 00:00:18", new InOutPoints(null, 18_000).Text(ms => TimeSpan.FromMilliseconds(ms).ToString(@"hh\:mm\:ss")));
        });

        T.Run("export: handles are grabbed and moved within limits", () =>
        {
            var p = new InOutPoints(12_000, 18_000);
            T.Equal(PointHandle.In, p.HandleAt(0.21, Lo, Hi, 0.03));
            T.Equal(PointHandle.Out, p.HandleAt(0.79, Lo, Hi, 0.03));
            T.Check(p.HandleAt(0.5, Lo, Hi, 0.03) is null);
            T.Check(default(InOutPoints).HandleAt(0.5, Lo, Hi, 1) is null);
            p = p.Move(PointHandle.In, 19_000, Lo, Hi); // stops short of the out point
            T.Equal(18_000 - InOutPoints.MinimumMs, p.InMs);
            p = p.Move(PointHandle.Out, 1_000, Lo, Hi); // stops past the in point
            T.Equal(p.InMs + InOutPoints.MinimumMs, p.OutMs);
            p = p.Move(PointHandle.In, -5, Lo, Hi);
            T.Equal(10_000L, p.InMs);
        });

        T.Run("export: requests are the JSON the core reads", () =>
        {
            var cam = new RecordingCamera("d-example", 2);
            var shared = new ExportOptions("/Volumes/Out Folder/with \"quotes\"", true);
            using JsonDocument trim = JsonDocument.Parse(ExportRequest.Trim("d1", [cam], 5, 9, true, shared));
            JsonElement t = trim.RootElement;
            T.Check(t.GetProperty("kind").GetString() == "trim" && t.GetProperty("cut").GetString() == "exact" && t.GetProperty("share_safe").GetBoolean());
            T.Equal("/Volumes/Out Folder/with \"quotes\"", t.GetProperty("dest").GetString());
            T.Equal(5L, t.GetProperty("in_ms").GetInt64());
            T.Equal(2, t.GetProperty("cameras")[0].GetProperty("channel").GetInt32());
            using JsonDocument plain = JsonDocument.Parse(ExportRequest.Trim("d1", null, 5, 9, false, new ExportOptions()));
            JsonElement q = plain.RootElement;
            T.Check(!q.TryGetProperty("cameras", out _) && !q.TryGetProperty("dest", out _) && !q.TryGetProperty("share_safe", out _));
            T.Equal("keyframe", q.GetProperty("cut").GetString());
            using JsonDocument grid = JsonDocument.Parse(ExportRequest.Edit("4up", "d1", (1, 2), new ExportOptions()));
            T.Check(grid.RootElement.GetProperty("kind").GetString() == "grid" && grid.RootElement.GetProperty("layout").GetString() == "4up");
            T.Equal(2L, grid.RootElement.GetProperty("out_ms").GetInt64());
            using JsonDocument follow = JsonDocument.Parse(ExportRequest.Edit(null, "d1", null, new ExportOptions()));
            T.Check(follow.RootElement.GetProperty("kind").GetString() == "follow" && !follow.RootElement.TryGetProperty("in_ms", out _)
                && !follow.RootElement.TryGetProperty("canvas", out _));
            // Whole seconds: the start rounds down, the end up.
            using JsonDocument range = JsonDocument.Parse(ExportRequest.Range(1_500, 4_001, [cam], "  ", new ExportOptions()));
            T.Check(range.RootElement.GetProperty("start").GetInt64() == 1 && range.RootElement.GetProperty("end").GetInt64() == 5
                && !range.RootElement.TryGetProperty("name", out _));
            T.Equal("main", range.RootElement.GetProperty("cameras")[0].GetProperty("tier").GetString());
            using JsonDocument named = JsonDocument.Parse(ExportRequest.Range(0, 1_000, [cam], "Gate", new ExportOptions()));
            T.Equal("Gate", named.RootElement.GetProperty("name").GetString());
            using JsonDocument stills = JsonDocument.Parse(ExportRequest.Stills(7, [3, 4], PictureFormat.Jpeg, new ExportOptions()));
            T.Check(stills.RootElement.GetProperty("playback").GetInt64() == 7 && stills.RootElement.GetProperty("tiles").GetArrayLength() == 2
                && stills.RootElement.GetProperty("format").GetString() == "jpeg");
            using JsonDocument file = JsonDocument.Parse(ExportRequest.Still("d1", cam, 77, false, PictureFormat.Png, new ExportOptions()));
            T.Check(file.RootElement.GetProperty("at_ms").GetInt64() == 77 && !file.RootElement.GetProperty("exact").GetBoolean()
                && file.RootElement.GetProperty("download").GetString() == "d1");
        });

        T.Run("export: plans parse", () =>
        {
            const string trim = """
                {"version":1,"kind":"trim","download":"d1","start_ms":1000,"end_ms":4000,"share_safe":true,
                 "outputs":[{"source":0,"camera":{"device":"d-example","channel":1,"lens":0},"name":"Camera A",
                 "file":"/x/a.mp4","source_in_ms":2000,"source_out_ms":4500,"copy_possible":true,"suggested_file":"/y/a trim.mp4"}]}
                """;
            TrimPlan plan = T.NotNull(TrimPlan.Parse(trim));
            T.Check(plan.Outputs[0].SourceInMs == 2000 && plan.Outputs[0].CopyPossible && plan.ShareSafe);
            T.Check(TrimPlan.Parse("""{"kind":"follow","download":"d","start_ms":0,"end_ms":1,"outputs":[]}""") is null);
            T.Check(TrimPlan.Parse(trim.Replace("\"source_out_ms\":4500", "\"source_out_ms\":1500", StringComparison.Ordinal)) is null);
            StillPlan stills = T.NotNull(StillPlan.Parse(
                """{"stills":[{"tile":11,"camera":{"device":"d-example","channel":0,"lens":0},"name":"Camera A","generation":4,"position_ms":99,"suggested_file":"/z/a.png"}]}"""));
            T.Check(stills.Stills[0].Tile == 11UL && stills.Stills[0].PositionMs == 99 && stills.Stills[0].File is null);
            T.Check(StillPlan.Parse("""{"stills":[]}""") is null);
            T.Check(EditExportPlan.Parse("""{"kind":"grid","download":"d","start_ms":5,"end_ms":9,"suggested_file":"/a/b.mp4"}""") is { StartMs: 5, EndMs: 9 });
            T.Check(EditExportPlan.Parse("""{"kind":"trim","download":"d","start_ms":5,"end_ms":9}""") is null);
        });

        T.Run("export: output size, frame count and times", () =>
        {
            T.Equal((1920, 1080), ExportGeometry.FollowSize([(640, 360), (2560, 1440)]));
            T.Equal((640, 360), ExportGeometry.FollowSize([(640, 360), (641, 360)]));
            T.Equal((1920, 1080), ExportGeometry.Fitted(3840, 2160));
            T.Equal((1000, 1000), ExportGeometry.Fitted(1000, 1001));
            T.Equal((1280, 720), ExportGeometry.Fitted(0, 0));
            T.Equal(120, ExportGeometry.FrameCount(10_000, 14_000));
            T.Equal(10_000L, ExportGeometry.FrameTime(10_000, 14_000, 0));
            T.Equal(11_000L, ExportGeometry.FrameTime(10_000, 14_000, 30));
            T.Equal(13_966L, ExportGeometry.FrameTime(10_000, 14_000, 119));
            T.Equal(14_000L, ExportGeometry.FrameTime(10_000, 14_000, 500));
            T.Equal(1, ExportGeometry.FrameCount(10_000, 10_010));
        });

        T.Run("export: sound pieces of one file join when they follow each other", () =>
        {
            List<AudioPiece> joined = AudioPiece.Join([
                new AudioPiece(0, 1_000, 2_000, 0),
                new AudioPiece(0, 2_000, 3_000, 1_000),
                new AudioPiece(0, 5_000, 6_000, 3_000),
                new AudioPiece(1, 6_000, 7_000, 4_000),
            ]);
            T.Equal(3, joined.Count);
            T.Equal(new AudioPiece(0, 1_000, 3_000, 0), joined[0]);
            T.Equal(new AudioPiece(0, 5_000, 6_000, 3_000), joined[1]);
        });

        T.Run("export: share-safe zeroes times, renames handlers, and clears user data without moving anything", () =>
        {
            byte[] mp4 = MiniMp4();
            string path = Path.Combine(Path.GetTempPath(), "nvrview-sharesafe-" + Guid.NewGuid().ToString("N")[..8] + ".mp4");
            try
            {
                File.WriteAllBytes(path, mp4);
                Mp4ShareSafe.Apply(path);
                byte[] after = File.ReadAllBytes(path);
                T.Equal(mp4.Length, after.Length);
                string text = Encoding.Latin1.GetString(after);
                foreach (string leak in new[] { "Media Foundation", "Core Media", "Lavf", "udta", "meta" })
                {
                    T.Check(!text.Contains(leak, StringComparison.Ordinal), leak);
                }
                T.Check(text.Contains("VideoHandler", StringComparison.Ordinal) && text.Contains("SoundHandler", StringComparison.Ordinal));
                T.Check(text.Contains("free", StringComparison.Ordinal));
                T.Check(!after.Contains((byte)0xAB), "times are zero");
                // Offsets stay right: the mdat is where it was.
                T.Equal(Encoding.Latin1.GetString(mp4).IndexOf("mdat", StringComparison.Ordinal), text.IndexOf("mdat", StringComparison.Ordinal));
                File.WriteAllBytes(path, [0, 0, 0, 8, (byte)'f', (byte)'r', (byte)'e', (byte)'e']);
                bool refused = false;
                try
                {
                    Mp4ShareSafe.Apply(path);
                }
                catch (Mp4ShareSafe.NotMp4Exception)
                {
                    refused = true;
                }
                T.Check(refused, "no movie box");
            }
            finally
            {
                File.Delete(path);
            }
        });

        T.Run("export: files are written partial and named free", () =>
        {
            string dir = Path.Combine(Path.GetTempPath(), "nvrview-free-" + Guid.NewGuid().ToString("N")[..8]);
            Directory.CreateDirectory(dir);
            try
            {
                string target = Path.Combine(dir, "clip.mp4");
                string partial = ExportFile.Partial(target);
                T.Equal(dir, Path.GetDirectoryName(partial));
                T.Check(Path.GetFileName(partial).StartsWith(".nvrview-export-", StringComparison.Ordinal) && partial.EndsWith(".mp4", StringComparison.Ordinal));
                File.WriteAllBytes(partial, MiniMp4());
                File.WriteAllText(target, "old");
                long bytes = ExportFile.Commit(partial, target, shareSafe: true);
                T.Check(bytes == MiniMp4().Length && !File.Exists(partial), "replaced when whole");
                T.Equal(Path.Combine(dir, "clip (2).mp4"), ExportFile.Free(target));
                T.Equal(Path.Combine(dir, "clip (3).mp4"), ExportFile.Free(target, taken: new HashSet<string> { "clip (2).mp4" }));
                ExportFile.Discard(partial);
            }
            finally
            {
                Directory.Delete(dir, recursive: true);
            }
        });

        T.Run("export: settings are kept between launches", () =>
        {
            string path = Path.Combine(Path.GetTempPath(), "nvrview-export-settings-" + Guid.NewGuid().ToString("N")[..8] + ".json");
            try
            {
                var s = new ExportSettings(path) { ShareSafe = true, TrimExact = true, Codec = ExportCodec.Hevc, Folder = "/some/folder" };
                s.Save();
                var back = new ExportSettings(path);
                back.Load();
                T.Check(back.ShareSafe && back.TrimExact && back.Codec == ExportCodec.Hevc && back.Folder == "/some/folder");
                var none = new ExportSettings(path + ".missing");
                none.Load();
                T.Check(!none.ShareSafe && !none.TrimExact && none.Codec == ExportCodec.H264 && none.Folder is null);
            }
            finally
            {
                File.Delete(path);
            }
        });

        T.Run("export: the download list shows a range", () =>
        {
            var rig = new Rig();
            try
            {
                rig.Browser.Click(rig.Browser.DayIncidents.OrderBy(i => i.Start).First().Key, extend: false);
                rig.Work();
                rig.Browser.Export.BeginRangeDownload();
                RangeDownloadRequest request = T.NotNull(rig.Browser.Export.RangeRequest);
                T.Check(request.Cameras.Count >= 1);
                request.Cameras = [rig.Cameras[0], rig.Cameras[1]];
                rig.Browser.Export.DownloadRange(request);
                rig.Work();
                T.Check(rig.Browser.Export.RangeRequest is null && rig.Browser.Center == RecordingsCenter.Downloads);
                DownloadProgress d = T.NotNull(rig.Service.Downloads.LastOrDefault(x => x.Range is not null));
                T.Equal("done", d.State);
                T.Equal(2, d.Cameras.Count);
                T.Check(d.Title().StartsWith("Range ", StringComparison.Ordinal) && d.RangeText!.Contains(" to ", StringComparison.Ordinal));
                T.Equal(request.StartMs / 1000, d.Range!.Start);
                T.Equal(-Math.Floor(-request.EndMs / 1000.0), (double)d.Range.End);
                request.Cameras = [];
                rig.Browser.Export.DownloadRange(request);
                T.Equal("Choose at least one camera", rig.Browser.Message);
            }
            finally
            {
                rig.Dispose();
            }
        });

        T.Run("export: a trim of a downloaded incident is a keyframe copy in the chosen file", () =>
        {
            var rig = new Rig();
            try
            {
                ExportController export = rig.Browser.Export;
                Incident first = rig.PlayAndDownload();
                (long lo, _) = rig.Scrub;
                // No points yet.
                export.TrimAsync(null).GetAwaiter().GetResult();
                T.Equal("Set an in point and an out point first (I and O)", rig.Browser.Message);
                export.RawPoints = new InOutPoints(lo + 3_000, lo + 9_000);
                T.Check(export.PointsWindow == (lo + 3_000, lo + 9_000));
                export.ShareSafe = true;
                export.TrimAsync(null).GetAwaiter().GetResult();
                ExportJob job = T.NotNull(export.Job);
                job.Completion.GetAwaiter().GetResult();
                T.Check(!job.Running && job.Files.Count >= 1, job.Result);
                T.Check(rig.Writers.Trims.Count == job.Files.Count && rig.Writers.Trims.All(t => !t.Exact && t.Output.CopyPossible));
                foreach (string file in job.Files)
                {
                    T.Check(File.Exists(file) && Path.GetDirectoryName(file) == rig.Folder, file);
                    string text = Encoding.Latin1.GetString(File.ReadAllBytes(file));
                    T.Check(!text.Contains("Media Foundation", StringComparison.Ordinal) && text.Contains("VideoHandler", StringComparison.Ordinal), "share-safe applied");
                }
                T.Check(Directory.GetFiles(rig.Folder, ".nvrview-export-*").Length == 0, "no partial file is left");
                T.Equal(rig.Folder, export.Folder);
                T.Check(export.Busy is false);
                // The same names again: the second export does not replace the first.
                export.DismissJob();
                export.ShareSafe = false;
                export.TrimExact = true;
                export.TrimAsync(null).GetAwaiter().GetResult();
                export.Job!.Completion.GetAwaiter().GetResult();
                T.Check(rig.Writers.Trims[^1].Exact && !rig.Writers.Trims[^1].Output.CopyPossible, "exact trims re-encode");
                T.Check(first.Key.Length > 0);
            }
            finally
            {
                rig.Dispose();
            }
        });

        T.Run("export: a canceled or failed trim leaves no file", () =>
        {
            var rig = new Rig();
            try
            {
                ExportController export = rig.Browser.Export;
                rig.PlayAndDownload();
                (long lo, _) = rig.Scrub;
                export.RawPoints = new InOutPoints(lo + 3_000, lo + 9_000);
                rig.Writers.CancelMidway = true;
                export.TrimAsync(null).GetAwaiter().GetResult();
                export.Job!.Completion.GetAwaiter().GetResult();
                T.Equal("Export canceled", export.Job.Result);
                T.Check(export.Job.Files.Count == 0 && Directory.GetFiles(rig.Folder).Length == 0, "canceled: nothing written");
                export.DismissJob();
                rig.Writers.CancelMidway = false;
                rig.Writers.FailTrim = true;
                export.TrimAsync(null).GetAwaiter().GetResult();
                export.Job!.Completion.GetAwaiter().GetResult();
                T.Equal("The cut failed", export.Job.Result);
                T.Check(Directory.GetFiles(rig.Folder).Length == 0, "failed: nothing written");
                // A canceled picker exports nothing and asks only once.
                export.DismissJob();
                rig.Writers.FailTrim = false;
                rig.Dialogs.Cancel = true;
                int asked = rig.Dialogs.Asked.Count;
                export.TrimAsync(null).GetAwaiter().GetResult();
                T.Check(export.Job is null && rig.Dialogs.Asked.Count == asked + 1);
                // One export at a time.
                rig.Dialogs.Cancel = false;
            }
            finally
            {
                rig.Dispose();
            }
        });

        T.Run("export: a trim of an incident that is not downloaded downloads its range first", () =>
        {
            var rig = new Rig();
            try
            {
                ExportController export = rig.Browser.Export;
                Incident first = rig.Browser.DayIncidents.OrderBy(i => i.Start).First();
                rig.Browser.Click(first.Key, extend: false);
                rig.Work();
                (long lo, _) = rig.Scrub;
                export.RawPoints = new InOutPoints(lo + 3_000, lo + 9_000);
                export.TrimAsync(null).GetAwaiter().GetResult();
                T.Check(export.Job is null, "the trim waits for the range");
                DownloadProgress ranged = T.NotNull(rig.Service.Downloads.FirstOrDefault(d => d.Range is not null));
                T.Check(ranged.State is "queued" or "running");
                rig.Work();
                ExportJob job = T.NotNull(export.Job);
                job.Completion.GetAwaiter().GetResult();
                T.Check(job.Files.Count >= 1 && rig.Writers.Trims.Count >= 1, job.Result);
            }
            finally
            {
                rig.Dispose();
            }
        });

        T.Run("export: pictures from a downloaded file, one per camera", () =>
        {
            var rig = new Rig();
            try
            {
                ExportController export = rig.Browser.Export;
                rig.PlayAndDownload();
                DownloadProgress d = rig.Service.Downloads.Last(x => x.State == "done");
                (long lo, _) = rig.Scrub;
                RecordingCamera[] cameras = [.. d.Cameras.Select(c => c.Camera)];
                export.SaveStillsFromFilesAsync(d.Download, cameras, lo + 2_500).GetAwaiter().GetResult();
                ExportJob job = T.NotNull(export.Job);
                job.Completion.GetAwaiter().GetResult();
                T.Equal(cameras.Length, job.Files.Count);
                T.Check(job.Files.Distinct().Count() == cameras.Length && job.Files.All(File.Exists));
                T.Check(rig.Writers.FileStills.All(s => s.Ms >= 0 && s.Format == PictureFormat.Png));
                // Nothing to save without a playback.
                export.DismissJob();
                rig.Browser.Player.Close();
                export.SaveStillsAsync(eachCamera: true).GetAwaiter().GetResult();
                T.Equal("Play an incident or a camera to save its picture", rig.Browser.Message);
            }
            finally
            {
                rig.Dispose();
            }
        });
    }
}
