// Self-test steps of export from the Recordings window (docs/UX.md "Recordings", "Export"; the same IDs as the Mac
// app's). They run on the fakes with synthetic MP4 files (SyntheticRecording) written where the fake's plans say the
// downloaded files are: a keyframe trim of an incident's download, the same share-safe, a range download of two
// cameras, and a picture of each camera of that download. The save and folder pickers are answered by a script, the
// files are read back with Media Foundation, and the pictures checked for the camera and frame they show.

using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    // Answers the pickers with files in a throwaway folder, and remembers what was asked.
    private sealed class ScriptedDialogs(string folder) : IExportDialogs
    {
        public readonly List<string> Asked = [];

        public Task<string?> SaveFile(string suggestedName, string? start, string extension)
        {
            Asked.Add($"save {suggestedName}");
            return Task.FromResult<string?>(Path.Combine(folder, suggestedName));
        }

        public Task<string?> ChooseFolder(string message, string? start)
        {
            Asked.Add("folder");
            return Task.FromResult<string?>(folder);
        }
    }

    // What a file's picture at a time shows, read with Media Foundation.
    private static (int? Color, int? Frame)? LookFile(string path, long ms)
    {
        RenderedImage? image = MfExport.Picture(path, ms, exact: true);
        return image is null ? null : SyntheticRecording.Read(image);
    }

    private static async Task<bool> JobDone(RecordingsWindow w) =>
        await Until(() => w.Browser.Export.Job is { Running: false }, 90);

    private async Task ExportSteps(RecordingsWindow w, FakeRecordings fake, RecordingsService service)
    {
        RecordingsBrowser b = w.Browser;
        RecordingsPlayer p = b.Player;
        ExportController export = b.Export;
        if (b.PlayingIncident is not { } incident || b.ScrubRange is not { } scrub || p.Cameras.Count == 0)
        {
            _problems.Add("export: no playing incident to export from");
            return;
        }
        string dir = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-export-{Guid.NewGuid():N}");
        Directory.CreateDirectory(dir);
        var dialogs = new ScriptedDialogs(dir);
        IExportDialogs? realDialogs = export.Dialogs;
        bool wasShareSafe = export.ShareSafe, wasExact = export.TrimExact;
        export.Dialogs = dialogs;
        export.ShareSafe = false;
        export.TrimExact = false;
        try
        {
            RecordingCamera camera = p.Cameras[0];

            // parity: export-trim
            b.Download(incident.Key);
            bool downloaded = await Until(() =>
            {
                fake.CompleteRecordingWork();
                return service.FinishedDownload(incident.Key) is not null;
            }, 5);
            DownloadProgress? download = service.FinishedDownload(incident.Key);
            if (!downloaded || download is null)
            {
                _problems.Add("export: the incident did not download");
                return;
            }
            long lo = scrub.Start;
            export.RawPoints = new InOutPoints(lo + 2_500, lo + 5_000);
            Expect(export.PointsWindow == (lo + 2_500, lo + 5_000), "export: the points do not make the window asked for");
            w.Refresh();
            Expect(w.ScrubPointsShown == (lo + 2_500, lo + 5_000), "export: the scrubber does not show the in and out points");
            Planned<TrimPlan> planned = service.PlanTrim(ExportRequest.Trim(download.Download, [camera], lo + 2_500, lo + 5_000, false, new ExportOptions()));
            if (planned.Plan?.Outputs.Count != 1)
            {
                _problems.Add($"export: the fake planned no trim ({planned.Refusal?.Text})");
                return;
            }
            TrimOutput output = planned.Plan.Outputs[0];
            Expect(output.CopyPossible, "export: a keyframe trim should be a copy");
            bool wrote = SyntheticRecording.Write(output.File, camera.Channel, (int)Math.Min((output.SourceOutMs + 3_000) / 100, 1_000));
            Expect(wrote, "export: the synthetic recording could not be written");
            await export.TrimAsync([camera]);
            Expect(await JobDone(w), "export: the trim did not finish");
            ExportJob? job = export.Job;
            string[] files = job?.Files.ToArray() ?? [];
            if (files.Length != 1)
            {
                _problems.Add($"export: the trim wrote {files.Length} files ({job?.Result ?? "no message"})");
            }
            else
            {
                (long DurationMs, int Width, int Height)? probe = MfExport.Probe(files[0]);
                (int? Color, int? Frame)? first = LookFile(files[0], 0);
                double want = (output.SourceOutMs - output.SourceInMs) / 1000.0;
                long expectedFrame = output.SourceInMs / 100;
                Say($"export: trim {Path.GetFileName(files[0])} {(probe is { } pr ? $"{pr.DurationMs / 1000.0:0.0} s, {pr.Width}x{pr.Height}" : "unreadable")}, first {first}, expected {want:0.0} s from file ms {output.SourceInMs}");
                if (probe is not { } found)
                {
                    _problems.Add("export: the trimmed file could not be read");
                }
                else
                {
                    Expect(Math.Abs(found.DurationMs / 1000.0 - want) < 0.5, $"export: the trim lasts {found.DurationMs / 1000.0:0.0} s, expected {want:0.0}");
                    Expect(found.Width == 640 && found.Height == 360, $"export: the trim is {found.Width}x{found.Height}, expected 640x360");
                }
                // A keyframe copy starts at the keyframe at or before the in point.
                Expect(first is { Color: { } c, Frame: { } f } && c == camera.Channel % 4 && f <= expectedFrame && f >= expectedFrame - 20,
                    $"export: the trim starts at {first}, expected camera {camera.Channel % 4} frame {expectedFrame} or up to 20 before");
            }
            Expect(Directory.GetFiles(dir, ".nvrview-export-*").Length == 0, "export: a partial file is left");
            export.DismissJob();

            // A canceled export leaves no file: cancel at once, an exact trim (it re-encodes).
            string[] before = Directory.GetFiles(dir);
            export.TrimExact = true;
            await export.TrimAsync([camera]);
            export.Job?.Cancel();
            await JobDone(w);
            string[] afterCancel = Directory.GetFiles(dir);
            bool canceled = export.Job?.Result == "Export canceled";
            Say($"export: exact trim canceled at once: {export.Job?.Result}; files {before.Length} to {afterCancel.Length}");
            Expect(Directory.GetFiles(dir, ".nvrview-export-*").Length == 0 && (canceled ? afterCancel.SequenceEqual(before) : afterCancel.Length >= before.Length),
                "export: a canceled trim left a file");
            export.TrimExact = false;
            export.DismissJob();

            // parity: export-share-safe
            export.ShareSafe = true;
            await export.TrimAsync([camera]);
            Expect(await JobDone(w), "export: the share-safe trim did not finish");
            if (export.Job?.Files.FirstOrDefault() is not { } safe)
            {
                _problems.Add($"export: the share-safe trim wrote nothing ({export.Job?.Result ?? "no message"})");
            }
            else
            {
                byte[] bytes = File.ReadAllBytes(safe);
                string text = System.Text.Encoding.Latin1.GetString(bytes);
                string[] leaks = new[] { "Media Foundation", "udta", "meta", "Lavf" }.Where(s => text.Contains(s, StringComparison.Ordinal)).ToArray();
                int mvhd = text.IndexOf("mvhd", StringComparison.Ordinal);
                bool zeroTimes = mvhd > 0 && bytes.Skip(mvhd + 8).Take(8).All(x => x == 0);
                Say($"export: share-safe trim {Path.GetFileName(safe)}, {bytes.Length} bytes, leaks [{string.Join(", ", leaks)}], movie times zero {zeroTimes}");
                Expect(bytes.Length > 0 && leaks.Length == 0 && zeroTimes, $"export: the share-safe trim holds {string.Join(", ", leaks)} or has times set");
                Expect(MfExport.Probe(safe) is not null, "export: the share-safe trim cannot be read");
                Expect(!Path.GetFileName(safe).Contains("Camera 1", StringComparison.Ordinal) && !Path.GetFileName(safe).Contains("Camera 2", StringComparison.Ordinal)
                    && !Path.GetFileName(safe).Contains("Camera 3", StringComparison.Ordinal), $"export: the share-safe file is named {Path.GetFileName(safe)}");
            }
            export.ShareSafe = false;
            export.DismissJob();

            // parity: export-range-download
            export.RawPoints = new InOutPoints(lo + 1_000, lo + 4_200);
            export.BeginRangeDownload();
            if (export.RangeRequest is not { } request)
            {
                _problems.Add("export: the range sheet did not open");
                return;
            }
            Expect(request.Cameras.Count == p.Cameras.Count, $"export: the range sheet offers {request.Cameras.Count} cameras, expected {p.Cameras.Count}");
            RecordingCamera[] cameras = [.. b.Cameras.Take(2)];
            request.Cameras = new HashSet<RecordingCamera>(cameras);
            export.DownloadRange(request);
            long wantStart = DeviceTime.FloorDiv(lo + 1_000, 1000), wantEnd = -DeviceTime.FloorDiv(-(lo + 4_200), 1000);
            bool ranged = await Until(() =>
            {
                fake.CompleteRecordingWork();
                return service.Downloads.Any(d => d.Range is not null && d.State == "done");
            }, 5);
            DownloadProgress? range = service.Downloads.LastOrDefault(d => d.Range is not null);
            Say($"export: range download {range?.RangeText ?? "none"}, {range?.Cameras.Count ?? 0} cameras, {range?.State ?? "?"}");
            Expect(ranged && range?.Range == new DownloadRange(wantStart, wantEnd) && range.Cameras.Count == 2,
                $"export: the range download is {range?.Range} with {range?.Cameras.Count ?? 0} cameras ({range?.State ?? "none"}), expected {wantStart}-{wantEnd} with 2");
            Expect(b.Center == RecordingsCenter.Downloads, "export: a range download did not show the downloads");
            w.Refresh();
            Expect(w.DownloadCardTitles().Any(t => t.StartsWith("Range ", StringComparison.Ordinal)), "export: the downloads list does not say Range");
            b.Center = RecordingsCenter.Player;
            w.Refresh();

            // parity: export-stills
            if (range is { State: "done" })
            {
                long at = wantStart * 1000 + 1_500;
                var items = new List<StillItem>();
                foreach (RecordingCamera c in range.Cameras.Select(x => x.Camera))
                {
                    Planned<StillPlan> plan = service.PlanStills(ExportRequest.Still(range.Download, c, at, true, PictureFormat.Png, new ExportOptions()));
                    if (plan.Plan is null)
                    {
                        _problems.Add($"export: no picture plan ({plan.Refusal?.Text})");
                        continue;
                    }
                    items.AddRange(plan.Plan.Stills);
                    StillItem s = plan.Plan.Stills[0];
                    if (s.File is not null)
                    {
                        Expect(SyntheticRecording.Write(s.File, c.Channel, 40), "export: the synthetic recording could not be written");
                    }
                }
                await export.SaveStillsFromFilesAsync(range.Download, range.Cameras.Select(x => x.Camera), at);
                Expect(await JobDone(w), "export: the pictures were not saved");
                string[] pictures = export.Job?.Files.ToArray() ?? [];
                Say($"export: a picture per camera {string.Join(", ", pictures.Select(Path.GetFileName))}");
                Expect(pictures.Length == items.Count && pictures.Distinct().Count() == pictures.Length,
                    $"export: a picture per camera wrote {pictures.Length} files, expected {items.Count} ({export.Job?.Result ?? "no message"})");
                for (int i = 0; i < pictures.Length && i < items.Count; i++)
                {
                    RenderedImage? image = await PictureSaver.DecodeAsync(pictures[i]);
                    if (image is null)
                    {
                        _problems.Add($"export: {Path.GetFileName(pictures[i])} could not be read back");
                        continue;
                    }
                    (int? color, int? frame) = SyntheticRecording.Read(image);
                    long expected = (items[i].SourceInMs ?? 0) / 100;
                    Expect(image.Width == 640 && image.Height == 360, $"export: {Path.GetFileName(pictures[i])} is {image.Width}x{image.Height}");
                    Expect(color == items[i].Camera.Channel % 4 && frame is { } f && Math.Abs(f - expected) <= 1,
                        $"export: {Path.GetFileName(pictures[i])} shows camera {color} frame {frame}, expected camera {items[i].Camera.Channel % 4} frame {expected}");
                }
                export.DismissJob();
            }
            else
            {
                _problems.Add("export: no finished range download for the pictures");
            }
        }
        finally
        {
            export.Dialogs = realDialogs;
            export.ShareSafe = wasShareSafe;
            export.TrimExact = wasExact;
            export.RawPoints = default;
            export.DismissJob();
            try
            {
                Directory.Delete(dir, recursive: true);
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
            }
        }
    }
}
