// Self-test area "recordings": the Recordings window: browsing, playback, edits, export, and sound analysis (export steps: SelfTest.Export.cs).

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
    // 11d. The Recordings window (docs/UX.md "Recordings"). First the entry points on the real service (the header's
    // menu, one window, the playback closed with it); then the steps of the macOS self-test on a model over the fakes,
    // shown in the same window class (the mock-nvr has no playback, and a made-up store makes the numbers exact).
    private async Task RecordingsSteps(Viewer v)
    {
        if (app.Recordings is not { Available: true })
        {
            Say("recordings: the core has none; skipped");
            return;
        }
        List<string> overflow = Texts(app.Windows[0].Header.BuildOverflow().Items);
        Expect(overflow.Contains("Recordings"), "the header's menu lacks Recordings");
        RecordingsWindow? first = app.OpenRecordings();
        bool loaded = first is not null && await Until(() => first.IsLoaded, 5);
        RecordingsWindow? again = app.OpenRecordings();
        Say($"recordings: window {(first is null ? "not opened" : "opened")}, loaded {loaded}, second open the same {ReferenceEquals(first, again)}, created {RecordingsWindow.Created}");
        Expect(loaded && ReferenceEquals(first, again) && RecordingsWindow.Created == 1, "the Recordings window did not open once");
        first?.Close();
        await Until(() => RecordingsWindow.Current is null, 5);

        var ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        RecordingsService service = null!;
        var fake = new FakeRecordings(json => service.Route(Encoding.UTF8.GetBytes(json))) { RecordingWorkIsManual = true };
        var playback = new FakePlayback(json => service.Route(Encoding.UTF8.GetBytes(json)), fake) { RecordingWorkIsManual = true };
        var host = new PlainPlaybackHost(playback);
        var trashed = new List<string>();
        var revealed = new List<IReadOnlyList<string>>();
        service = new RecordingsService(fake, host)
        {
            Post = a => _ = ui.TryEnqueue(() => a()),
            Now = () => DateTimeOffset.UtcNow,
            Trasher = trashed.Add,
            Revealer = revealed.Add,
            CameraNamer = c => $"Camera {c.Channel + 1}",
        };
        ThumbnailDecodeQueue thumbnailQueue = App.UseThumbnailDecoder(service);
        service.Start(Path.Combine(Path.GetTempPath(), "nvrview-selftest-incidents-" + Environment.ProcessId));
        Task<bool> Playing(Func<bool> condition, double seconds) => Until(() =>
        {
            playback.CompletePlaybackWork();
            return condition();
        }, seconds);

        // parity: recordings-enable
        RecordingCamera[] cams = [.. Enumerable.Range(0, 3).Select(c => new RecordingCamera("d-selftest", c))];
        foreach (RecordingCamera c in cams)
        {
            service.SetEnabled(c, true);
        }
        Expect(service.EnabledCameras.SequenceEqual(cams), "recordings: the chosen cameras are not the enabled ones");
        RecordingsWindow w = RecordingsWindow.Open(v, v.Host, service, background: true);
        RecordingsBrowser b = w.Browser;
        RecordingsPlayer p = b.Player;
        await Until(() => w.IsLoaded, 5);

        // parity: recordings-hold-today
        bool held = await Until(() => fake.Holds.TryGetValue(b.HoldId, out var h) && h.Count == 3 && b.Lanes.Count == 3, 5);
        Say($"recordings: hold {b.HoldId} on {(fake.Holds.TryGetValue(b.HoldId, out var held3) ? held3.Count : 0)} cameras, {b.Lanes.Count} lanes");
        Expect(held && b.Holding, "recordings: today is not held");

        // parity: recordings-calendar-lanes
        bool shown = await Until(() => b.RecordedDays.Contains(b.Day) && b.Lanes.All(l => l.Spans.Count > 0) && b.DayIncidents.Count == 6 && w.IncidentRows().Count == 6, 5);
        int events = b.Lanes.Sum(l => l.Events.Count);
        Say($"recordings: calendar {b.RecordedDays.Count} days, {events} events, {b.DayIncidents.Count} incidents, {w.IncidentRows().Count} rows");
        Expect(shown && events == 6, "recordings: the calendar, lanes, or incident list are not what the store holds");
        Expect(w.CalendarDays().Any(d => d.Day == b.Day && d.Recorded && d.Enabled), "recordings: the calendar does not mark today");
        if (b.Lanes[0].Events.Count > 0)
        {
            b.Select(b.Lanes[0].Events[0]);
            Expect(b.Selected?.Involves(b.Lanes[0].Camera) == true, "recordings: a timeline click selected nothing");
            b.Selection = new HashSet<string>();
            p.Close();
        }

        // parity: recordings-navigate-day
        long today = b.Day;
        b.PreviousDay();
        Expect(!b.Holding && !fake.Holds.ContainsKey(b.HoldId), "recordings: yesterday is held");
        bool yesterday = await Until(() => b.DayIncidents.Count == 6 && b.RecordedDays.Contains(today - DeviceTime.Day), 5);
        b.NextDay();
        bool heldBack = b.Day == today && fake.Holds.TryGetValue(b.HoldId, out var heldAgain) && heldAgain.Count == 3;
        Say($"recordings: yesterday {(yesterday ? "has" : "lacks")} its incidents; today held again {heldBack}");
        Expect(yesterday && heldBack, "recordings: navigating a day did not ask for it or hold today again");
        await Until(() => b.DayIncidents.Count == 6, 5);

        // parity: recordings-thumbnail
        await RecordingsThumbnails(service, fake, b, w);

        // parity: recordings-lock
        Incident? vehicle = b.DayIncidents.FirstOrDefault(i => i.Kinds.SequenceEqual(["vehicle"]));
        if (vehicle is not null)
        {
            b.SetLocked(vehicle.Key, true);
            bool locked = await Until(() => b.DayIncidents.FirstOrDefault(i => i.Key == vehicle.Key)?.Locked == true && service.Downloads.Count == 1, 5);
            Say($"recordings: locked; {service.Notice ?? "no message"}; downloads {service.Downloads.Count}");
            Expect(locked && service.Notice?.Contains("downloading", StringComparison.Ordinal) == true, "recordings: locking did not queue the download and say so");
            b.LockedOnly = true;
            Expect(b.Incidents.Select(i => i.Key).SequenceEqual([vehicle.Key]), "recordings: the Locked filter shows another list");
            b.LockedOnly = false;
            b.SetLocked(vehicle.Key, false);
            await Until(() => b.DayIncidents.FirstOrDefault(i => i.Key == vehicle.Key)?.Locked == false, 5);
            Expect(service.Downloads.Count == 1, "recordings: unlocking touched the download");
        }
        else
        {
            _problems.Add("recordings: no vehicle incident to lock");
        }

        // parity: recordings-downloads
        if (service.Downloads.Count == 1)
        {
            DownloadProgress queued = service.Downloads[0];
            b.Center = RecordingsCenter.Downloads;
            w.Refresh();
            Expect(w.DownloadsShown && !w.PlayerColumnShown, "recordings: the downloads panel is not shown with its tab");
            service.Pause(queued.Download, true);
            await Until(() => service.Downloads.FirstOrDefault()?.Paused == true, 5);
            fake.CompleteRecordingWork();
            Expect(service.Downloads.FirstOrDefault()?.State != "done", "recordings: a paused download finished");
            // The panel follows the download's events (the core's shape, with null members): Downloading, then Done.
            var seen = new List<string>();
            void Seen() => seen.AddRange(w.DownloadCards().Select(c => c.State));
            service.DownloadsChanged += Seen;
            service.Pause(queued.Download, false);
            await Until(() => service.Downloads.FirstOrDefault()?.Paused == false, 5);
            fake.CompleteRecordingWork();
            bool done = await Until(() => service.Downloads.FirstOrDefault()?.State == "done", 5);
            service.DownloadsChanged -= Seen;
            Say($"recordings: the downloads panel showed {string.Join(", ", seen.Distinct())}");
            Expect(seen.Contains("Downloading") && seen.LastOrDefault() == "Done" && w.DownloadCards().Any(c => c.State == "Done"),
                "recordings: the downloads panel did not show the progress and Done");
            DownloadProgress? finished = service.Downloads.FirstOrDefault();
            if (done && finished is not null)
            {
                service.Reveal(finished);
                Expect(revealed.LastOrDefault()?.SequenceEqual(finished.Outputs()) == true, "recordings: Show in Explorer showed something else");
                service.Forget(finished.Download);
                Incident? other = b.DayIncidents.FirstOrDefault(i => i.Key != vehicle?.Key);
                if (other is not null)
                {
                    b.Download(other.Key);
                    await Until(() => service.Downloads.Count == 1 && service.Downloads[0].Download != finished.Download, 5);
                    string? folder = service.Downloads.FirstOrDefault()?.Folder;
                    if (folder is not null)
                    {
                        service.Trash(service.Downloads[0].Download);
                        bool moved = await Until(() => trashed.Count > 0, 5);
                        Say($"recordings: downloads done, forgotten, trashed {trashed.Count}");
                        Expect(moved && trashed.SequenceEqual([folder]) && service.Downloads.Count == 0, "recordings: the Recycle Bin did not get the folder");
                    }
                }
            }
            else
            {
                _problems.Add("recordings: the download did not finish after the resume");
            }
            b.Center = RecordingsCenter.Player;
            w.Refresh();
        }

        // parity: recordings-calendar-month
        int asked = fake.MonthRequests;
        long shownMonth = b.Month;
        b.ShowMonth(-1);
        long previous = b.Month;
        bool paged = await Until(() => fake.MonthRequests > asked && b.RecordedDays.Count > 0, 5);
        Say($"recordings: previous month asked {fake.MonthRequests - asked} time(s), {b.RecordedDays.Count} days marked");
        Expect(paged && fake.MonthRequests == asked + 1 && b.RecordedDays.All(d => DeviceTime.Month(d).Start == previous), "recordings: paging the calendar did not ask for that month alone");
        b.ShowMonth(1);
        b.ShowMonth(-1);
        Expect(fake.MonthRequests == asked + 1, "recordings: a month already asked was asked again");
        b.ShowMonth(1);
        Expect(b.Month == shownMonth, "recordings: the calendar did not come back to the shown day's month");

        // parity: recordings-camera-names
        List<string> names = [.. b.Lanes.Select(l => l.Name)];
        List<string> listed = [.. b.Cameras.Select(c => $"Camera {c.Channel + 1}")];
        Say($"recordings: camera names {string.Join(", ", names)}");
        Expect(names.SequenceEqual(listed), "recordings: the lanes are not named as the camera list names them");

        // parity: playback-open
        Incident? firstIncident = b.DayIncidents.OrderBy(i => i.Start).FirstOrDefault();
        if (firstIncident is null)
        {
            _problems.Add("recordings: no incident to play");
        }
        else
        {
            b.Click(firstIncident.Key, extend: false);
            bool open = await Playing(() => p.Playback is not null && p.Cells.Count == 1 && p.Cells[0].State == "playing", 5);
            long start = (firstIncident.Start - firstIncident.Margin) * 1000;
            Say($"recordings: playback {p.Playback} of {p.Cells.Count} cameras, {p.State}, opens {p.Opens}");
            Expect(open && p.PositionMs >= start && p.PositionMs <= start + 5_000 && p.Transport.IsPlaying, "recordings: the selected incident did not start playing at its start less the margin");
            // The fake has no incident of three cameras; with nothing selected the player keeps the cameras added to it.
            b.Selection = new HashSet<string>();
            p.Add(cams.First(c => !p.Cameras.Contains(c)));
            p.Add(cams.First(c => !p.Cameras.Contains(c)));
            await Playing(() => p.Cells.Count == 3, 5);
            await Until(() => w.Renderer?.Layout.Tiles.Length == 3, 5);
            Expect(p.Cells.Count == 3 && w.Renderer?.Layout.Tiles.Length == 3, "recordings: the window's renderer does not draw the playback's three cameras");

            // parity: playback-transport
            int opens = p.Opens;
            p.Pause();
            await Playing(() => p.State == NvrView.Interop.NvPlaybackState.Paused, 5);
            long before = p.PositionMs;
            p.Skip(TransportState.SkipMs);
            Expect(p.PositionMs == Math.Min(before + TransportState.SkipMs, p.EndMs), "recordings: +10 s moved somewhere else");
            p.SetRate(4);
            Expect(PlaybackSpeed.Hint(p.Rate) == "Keyframes only, no sound" && p.Cells.All(c => !host.Audio.TryGetValue(c.Tile, out var a) || !a.Audible), "recordings: at 4x the hint or the sound is wrong");
            w.Refresh();
            Expect(w.TransportShown().Hint == "Keyframes only, no sound" && w.TransportShown().Speed == "4×", "recordings: the transport does not show the speed and its hint");
            p.SetRate(1);
            p.Play();
            await Playing(() => p.State == NvrView.Interop.NvPlaybackState.Playing && p.Rate == 1, 5);
            bool[] audible = [.. p.Cells.Select(c => host.Audio.TryGetValue(c.Tile, out var a) && a.Audible)];
            Say($"recordings: paused, skipped 10 s, 4x and back, playing; audible {string.Join(",", audible)}");
            Expect(audible.SequenceEqual([true, false, false]) && p.Opens == opens, "recordings: at 1x the first camera should be audible, with no reopen");

            // parity: playback-grid
            p.Click(1);
            PlayerGridMode emphasis = p.Mode;
            bool[] afterClick = [.. p.Cells.Select(c => host.Audio.TryGetValue(c.Tile, out var a) && a.Audible)];
            p.DoubleClick(1);
            PlayerGridMode single = p.Mode;
            p.DoubleClick(1);
            Expect(emphasis.Equals(new PlayerGridMode.Emphasis(1)) && single.Equals(new PlayerGridMode.Single(1)) && p.Mode.Equals(PlayerGridMode.Tiled), "recordings: the grid did not go large, single, and back");
            Expect(afterClick.SequenceEqual([false, true, false]), "recordings: the large camera's sound did not play");
            p.Muted = true;
            Expect(p.Cells.All(c => !host.Audio.TryGetValue(c.Tile, out var a) || !a.Audible), "recordings: muted, a camera is audible");
            p.Muted = false;
            w.Refresh();
            await Until(() => w.CellRects.Count(r => r is not null) == 3, 5);
            Expect(w.CellRects.Count(r => r is not null) == 3 && w.LabelTexts().Count >= 3, "recordings: the player's cells or labels are missing");

            // parity: playback-quality
            {
                // Auto in the grid is the sub-stream, the single view makes Auto pick the main stream, Low and High are forced
                // during playback and at speed, a zoom past 2x raises a grid tile, and the core reports each camera's tier while
                // the playback is never reopened, the position never goes back, and the zoom is kept. The macOS step of the same
                // ID is SelfTest+PlayerQuality.swift.
                RecordingCamera a = p.Cells[0].Camera, c = p.Cells[1].Camera;
                int count = p.Cells.Count;
                List<string> sub = [.. Enumerable.Repeat("sub", count)];
                List<string> Only(int i, string tier = "main")
                {
                    List<string> t = [.. sub];
                    t[i] = tier;
                    return t;
                }
                void ExpectTiers(string what, List<string> tiers)
                {
                    List<string> got = p.ReportedTiers();
                    Say($"player quality: {what}: {string.Join(", ", got)}");
                    Expect(got.SequenceEqual(tiers), $"player quality: {what}: the core reports {string.Join(", ", got)}, not {string.Join(", ", tiers)}");
                }
                long Position() => p.PositionMs;

                p.Mode = PlayerGridMode.Tiled;
                p.SetRate(1);
                foreach (PlayerCell cell in p.Cells)
                {
                    p.SetQuality(cell.Camera, PlayerQuality.Auto);
                }
                ExpectTiers("Auto in the grid", sub);
                Expect(p.Cells.All(cell => p.Streams.Label(cell.Camera) == "sub stream"), "player quality: the header does not say sub stream in the grid");

                // The zoom is kept across every change below.
                p.Views.ZoomAround(a, 0.5, 0.5, 1.5, ZoomBase.Whole);
                TileZoom zoom = p.Views.Zoom(a);
                int changes = p.StreamChanges;
                long last = Position();

                // The single view: Auto asks for the main stream of that camera only.
                p.DoubleClick(0);
                ExpectTiers("Auto in the single view", Only(0));
                Expect(p.Streams.Label(a) == "main stream", $"player quality: the header says {p.Streams.Label(a)} for the main stream");
                Expect(Position() >= last, "player quality: the position went back at the single view");
                last = Position();

                // Low during playback, then High on the other camera while it is hidden.
                p.SetQuality(a, PlayerQuality.Low);
                ExpectTiers("Low in the single view", sub);
                p.SetQuality(c, PlayerQuality.High);
                ExpectTiers("High on another camera", Only(1));
                Expect(p.Streams.Label(a) == "Low · sub stream", $"player quality: the header says {p.Streams.Label(a)} for Low");

                // At speed: Auto goes to the sub-stream, forced choices stay.
                p.SetQuality(a, PlayerQuality.Auto);
                List<string> both = [.. sub];
                both[0] = "main";
                both[1] = "main";
                ExpectTiers("Auto in the single view again", both);
                p.SetRate(4);
                ExpectTiers("4x: Auto is the sub-stream, High stays", Only(1));
                p.SetQuality(c, PlayerQuality.Auto);
                ExpectTiers("4x: all Auto", sub);
                p.SetRate(1);
                ExpectTiers("1x again: the single view's camera is main", Only(0));

                // Zoomed past 2x in the grid raises that tile.
                p.Mode = PlayerGridMode.Tiled;
                ExpectTiers("back to the grid", sub);
                p.Views.ZoomAround(c, 0.5, 0.5, 2.1, ZoomBase.Whole);
                p.RefreshStreams();
                ExpectTiers("zoomed past 2x in the grid", Only(1));
                p.Views.SetZoom(c, TileZoom.None);
                p.RefreshStreams();
                ExpectTiers("zoom reset", sub);

                Expect(p.Views.Zoom(a) == zoom, $"player quality: the zoom changed from {zoom} to {p.Views.Zoom(a)}");
                Expect(p.Opens == opens, "player quality: the playback was reopened");
                Expect(Position() >= last, "player quality: the position went back");
                Expect(p.StreamChanges > changes, "player quality: no stream change reached the core");
                p.Views.SetZoom(a, TileZoom.None);
            }

            // parity: playback-scrub
            Incident? playing = b.PlayingIncident;
            TimelineLane? lane = b.Lanes.FirstOrDefault(l => p.Cameras.Contains(l.Camera));
            if (playing is not null && lane is not null)
            {
                long target = playing.End + 2;
                int seeks = p.Seeks;
                b.LaneClick(lane.Camera, target);
                Expect(p.Seeks == seeks + 1 && p.PositionMs == target * 1000 && p.Opens == opens, "recordings: a lane click did not seek");
                // The scrubber seeks the same way (a drag shows keyframes, the release the exact picture).
                w.Refresh();
                (long rs, long re) = w.Scrubber.Range;
                Expect(re > rs, "recordings: the scrubber has no range");
                long point = rs + (re - rs) / 4;
                w.Scrubber.PointAt(point);
                Expect(Math.Abs(p.PositionMs - point) <= 1, "recordings: the scrubber did not seek to the point");
                await Playing(() => p.State == NvrView.Interop.NvPlaybackState.Playing, 5);
            }
            else
            {
                _problems.Add("recordings: no lane of a playing camera");
            }
        }

        await ExportSteps(w, fake, service);

        // parity: recordings-timeline-wheel-zoom
        // The lanes' wheel as the window gets it: a notch zooms around the pointer (its time stays under it), many
        // clamp at 48x, Shift and the wheel pans, and notches the other way go back to the whole day.
        {
            LanesView lanes = w.LanesShown;
            b.Timeline = new TimelineViewport();
            w.Refresh();
            double before = lanes.Viewport.FractionAtPointer(0.25);
            lanes.Wheel(120, horizontal: false, shift: false, 0.25);
            TimelineViewport one = b.Timeline;
            Expect(Math.Abs(one.Zoom - 1.25) < 1e-9 && Math.Abs(one.FractionAtPointer(0.25) - before) < 1e-9,
                $"recordings: a wheel notch made {one}, the time under the pointer moved");
            for (int i = 0; i < 40; i++)
            {
                lanes.Wheel(360, horizontal: false, shift: false, 0.25);
            }
            TimelineViewport zoomed = b.Timeline;
            Expect(zoomed.Zoom == TimelineViewport.MaxZoom, $"recordings: the wheel zoomed to {zoomed.Zoom}x, not 48x");
            lanes.Wheel(-120, horizontal: false, shift: true, 0.25);
            Expect(b.Timeline.Start > zoomed.Start && b.Timeline.Zoom == zoomed.Zoom, "recordings: Shift and the wheel did not pan later");
            for (int i = 0; i < 40; i++)
            {
                lanes.Wheel(-360, horizontal: false, shift: false, 0.25);
            }
            Say($"recordings: wheel zoom {one.Zoom}x, to 48x, panned from {zoomed.Start:0.000}, back to {b.Timeline}");
            Expect(b.Timeline == new TimelineViewport(), $"recordings: wheeling back left {b.Timeline}");
        }

        // parity: recordings-listen-for-sounds
        // A camera's switch reaches nv_sound_watch and its sound_watch state comes back; a fifth camera of one device
        // is refused; switching off takes it off the core's list.
        RecordingCamera[] heard = [.. Enumerable.Range(0, 5).Select(c => new RecordingCamera("d-selftest", c))];
        foreach (RecordingCamera c in heard.Take(4))
        {
            Expect(service.SetListening(c, true), $"recordings: listening to {c} was refused ({service.Notice})");
        }
        Expect(!service.SetListening(heard[4], true), "recordings: a fifth camera of a device listens");
        Expect(fake.Watched.SequenceEqual(heard.Take(4)), $"recordings: the core listens to {fake.Watched.Count} cameras");
        bool watching = await Until(() => service.ListenStates.TryGetValue(heard[0], out var st) && st.State == "watching", 5);
        Expect(watching, "recordings: no sound_watch state for a listening camera");
        foreach (RecordingCamera c in heard.Skip(1))
        {
            service.SetListening(c, false);
        }
        Expect(fake.Watched.SequenceEqual([heard[0]]), "recordings: switching off left other cameras listening");
        Say($"recordings: listening to {service.ListeningCameras.Count} camera, the fifth refused: {service.Notice}");

        // parity: recordings-loudness-lanes
        // A listening camera's lane has loudness (the fake answers a steady level), the others are blank; zooming in
        // asks again with a finer bucket.
        {
            b.Timeline = new TimelineViewport();
            b.ReloadLoudness(force: true);
            long whole = b.LoudnessBucket;
            List<RecordingCamera> loud = [.. b.Lanes.Where(l => b.LoudnessOf(l.Camera)?.Heard == true).Select(l => l.Camera)];
            Say($"recordings: loudness at {whole} s buckets, heard on {string.Join(", ", loud.Select(c => c.Channel))}");
            Expect(loud.SequenceEqual([heard[0]]), "recordings: loudness is not on the listening camera's lane alone");
            Expect(b.LoudnessOf(heard[0])?.Bucket == whole, "recordings: the loudness lane is not at the bucket asked");
            int reads = b.LoudnessReads;
            b.Timeline = new TimelineViewport(48);
            Expect(b.LoudnessReads == reads + 1 && b.LoudnessOf(heard[0])?.Bucket == b.LoudnessBucket && b.LoudnessBucket < whole,
                $"recordings: zooming in did not ask for finer loudness ({b.LoudnessBucket} s)");
            b.Timeline = new TimelineViewport();
            w.Refresh();
        }

        // parity: recordings-sound-labels
        // A label (as the app's classifier would put it) becomes an event from sound on its camera's lane and an
        // incident of its own, which the Sounds filter alone shows, with the sound badge.
        {
            long at = b.Day + 10 * 3600;
            fake.SoundLabelsPut($$"""{"camera":{{heard[0].Json}},"analyzer":"selftest-v1","start":{{at - 60}},"end":{{at + 60}},"labels":[{"kind":"dog_bark","start":{{at}},"end":{{at + 4}},"confidence":0.9,"label":"dog_bark"}]}""");
            bool labeled = await Until(() => b.DayIncidents.Any(i => i.Kinds.SequenceEqual(["dog_bark"])), 5);
            bool onLane = b.Lanes.FirstOrDefault(l => l.Camera == heard[0])?.Events.Any(e => e.Kind == "dog_bark" && e.Source == SoundKinds.LocalAudio) == true;
            b.SetKindFilter(KindFilter.Sound, true);
            List<string> soundsShown = [.. b.Incidents.Select(i => string.Join("+", i.Kinds))];
            IReadOnlyList<string> badge = b.Incidents.Count > 0 ? SoundKinds.Names(b.Incidents[0].Events) : [];
            Say($"recordings: a sound label on the lane {onLane}, the Sounds filter shows {string.Join(", ", soundsShown)}, badge {string.Join(", ", badge)}");
            Expect(labeled && onLane, "recordings: the sound label is not on its camera's lane");
            Expect(soundsShown.SequenceEqual(["dog_bark"]), "recordings: the Sounds filter does not show the sound alone");
            Expect(badge.SequenceEqual(["Dog barking"]), "recordings: the sound badge is wrong");
            b.SetKindFilter(KindFilter.Sound, false);
            service.SetListening(heard[0], false);
        }

        // parity: recordings-analyze-no-audio
        // Analyze Sounds with nothing downloaded for the day: the analysis fails no_audio with its message, the lane
        // shows no progress and no loudness.
        RecordingCamera? analyzed = null;
        if (b.Lanes.Count > 0)
        {
            RecordingCamera lane0 = b.Lanes[0].Camera;
            Expect(service.Downloads.Count == 0, $"recordings: {service.Downloads.Count} downloads before the analysis steps");
            b.AnalyzeSound(lane0);
            Expect(b.AnalysisOf(lane0)?.State == AnalysisState.Queued, "recordings: Analyze Sounds queued nothing on the lane");
            bool failed = await Until(() => { fake.CompleteRecordingWork(); return service.Analysis(lane0, b.Day)?.State == AnalysisState.Failed; }, 5);
            Say($"recordings: analyze with nothing downloaded: {service.Analysis(lane0, b.Day)?.Reason ?? "no result"}, {service.Notice ?? "no message"}");
            Expect(failed && service.Analysis(lane0, b.Day)?.Reason == "no_audio", "recordings: the analysis did not fail no_audio");
            Expect(service.Notice == SoundAnalysis.Failure("no_audio"), $"recordings: the no_audio message was {service.Notice ?? "missing"}");
            Expect(b.AnalysisOf(lane0) is null, "recordings: a failed analysis still shows progress");
            b.ReloadLoudness(force: true);
            Expect(b.LoudnessOf(lane0)?.Heard != true, "recordings: loudness appeared without audio");

            // parity: recordings-loudness-gaps
            // Empty stretches of a loudness lane say why, from what the core knows: nothing downloaded is "not analyzed
            // yet" and cannot be analyzed; a camera found without sound says "no audio"; the words are the label and the
            // hover text.
            {
                b.Timeline = new TimelineViewport();
                b.ReloadLoudness(force: true);
                List<LoudnessGap> gaps = [.. b.LoudnessOf(lane0)?.GapList ?? []];
                Say($"recordings: {gaps.Count} loudness gaps on the first lane: {string.Join(", ", gaps.Select(g => g.Reason))}");
                Expect(gaps.Count > 0, "recordings: the loudness lane has no gaps with reasons");
                foreach (LoudnessGap g in gaps.Where(g => g.Reason != "not_analyzed" || g.Analyzable || g.Label != "Not analyzed yet"))
                {
                    _problems.Add($"recordings: a gap with nothing downloaded says {g.Reason}, analyzable {g.Analyzable}");
                }
                if (gaps.Count > 0)
                {
                    Expect(gaps[0].Help == new LoudnessGap(gaps[0].Start, gaps[0].End, "not_analyzed").Help, $"recordings: the gap's hover text is {gaps[0].Help}");
                    // Not analyzable: the action does nothing, so no analysis is queued.
                    b.AnalyzeGap(gaps[0], lane0);
                    Expect(!service.IsAnalyzing(LoudnessScale.Microphone(lane0), b.Day), "recordings: Analyze this gap queued an analysis for a gap that has no video");
                }
                fake.SetLacksSound(lane0);
                b.ReloadLoudness(force: true);
                List<string> reasons = [.. (b.LoudnessOf(lane0)?.GapList ?? []).Select(g => g.Reason).Distinct()];
                Expect(reasons.SequenceEqual(["no_audio"]), $"recordings: a camera without sound has gaps {string.Join(", ", reasons)}");
                fake.SetLacksSound(lane0, false);
                b.ReloadLoudness(force: true);
            }

            // parity: recordings-loudness-height
            // The loudness strips' height: 28 points by default, dragged within 12 to 96, kept (ExportTests pins the settings
            // file), and the lanes' pane follows it.
            {
                double tall = RecordingsPanes.LanesHeight(900, 3, b.LoudnessHeight);
                b.LoudnessHeight = 500;
                Expect(b.LoudnessHeight == RecordingsPanes.LoudnessMax, $"recordings: a strip of 500 points is {b.LoudnessHeight}");
                b.LoudnessHeight = 3;
                Expect(b.LoudnessHeight == RecordingsPanes.LoudnessMin, $"recordings: a strip of 3 points is {b.LoudnessHeight}");
                b.LoudnessHeight = 40;
                b.KeepLoudnessHeight();
                Say($"recordings: strip height {b.LoudnessHeight}, lanes {tall} -> {RecordingsPanes.LanesHeight(900, 3, 40)}");
                Expect(b.LoudnessHeight == 40, $"recordings: the strip height is {b.LoudnessHeight}");
                Expect(RecordingsPanes.LanesHeight(900, 3, 40) > tall, "recordings: a taller strip did not grow the lanes");
                b.LoudnessHeight = RecordingsPanes.LoudnessHeight;
                b.KeepLoudnessHeight();
                w.Refresh();
            }

            // parity: recordings-analyze-day
            // After a download: progress, then loudness and a sound label on the lane, and the done message.
            Incident? toDownload = b.DayIncidents.FirstOrDefault(i => i.Kinds.SequenceEqual(["vehicle"])) ?? b.DayIncidents.FirstOrDefault(i => i.Cameras.Count > 0);
            if (toDownload is not null)
            {
                analyzed = toDownload.Cameras[0].Camera;
                b.Download(toDownload.Key);
                bool downloaded = await Until(() => { fake.CompleteRecordingWork(); return service.Downloads.Any(d => d.State == "done"); }, 5);
                Expect(downloaded, "recordings: the download for the analysis did not finish");

                // parity: recordings-analyze-gap
                // "Analyze this gap" on a gap the downloaded video covers: only that stretch is measured (progress,
                // then loudness inside it and nowhere else).
                {
                    RecordingCamera mic = LoudnessScale.Microphone(analyzed);
                    b.Timeline = new TimelineViewport();
                    b.ReloadLoudness(force: true);
                    LoudnessGap? gap = b.LoudnessOf(analyzed)?.GapList.FirstOrDefault(g => g.CanAnalyze);
                    Expect(gap is not null, "recordings: no analyzable gap after the download");
                    if (gap is not null)
                    {
                        long spanEnd = Math.Min(gap.End, gap.Start + 59);
                        Expect(gap.Help.Contains("Analyze this gap"), $"recordings: the analyzable gap's hover text is {gap.Help}");
                        b.AnalyzeGap(gap with { End = spanEnd }, analyzed);
                        Expect(service.IsAnalyzing(mic, b.Day), "recordings: Analyze this gap shows no progress");
                        bool gapDone = await Until(() => { fake.CompleteRecordingWork(); return service.Analysis(mic, b.Day)?.State == AnalysisState.Done; }, 5);
                        AnalysisEntry? gapEntry = service.Analysis(mic, b.Day);
                        Say($"recordings: analyzed the gap {gap.Start}-{spanEnd}: {gapEntry?.Seconds ?? 0} s");
                        Expect(gapDone && gapEntry is { Seconds: > 0 } && gapEntry.Seconds <= spanEnd - gap.Start + 1, $"recordings: the gap analysis measured {gapEntry?.Seconds} s of {spanEnd - gap.Start + 1}");
                        LoudnessLane? fine = service.Loudness(gap.Start - 600, spanEnd + 600, 1, [mic])?.Lanes.FirstOrDefault();
                        int outside = 0;
                        for (int i = 0; fine is not null && i < fine.Peak.Count; i++)
                        {
                            long t = fine.Start + i;
                            if (fine.Peak[i] is not null && (t < gap.Start || t > spanEnd))
                            {
                                outside++;
                            }
                        }
                        Expect(fine?.Heard == true, "recordings: no loudness inside the analyzed gap");
                        Expect(outside == 0, $"recordings: {outside} seconds of loudness outside the analyzed gap");
                        Expect(b.AnalysisOf(analyzed) is null, "recordings: a finished gap analysis still shows progress");
                    }
                }
                b.AnalyzeSound(analyzed);
                Expect(service.IsAnalyzing(analyzed, b.Day), "recordings: Analyze Sounds shows no progress");
                bool done = await Until(() => { fake.CompleteRecordingWork(); return service.Analysis(analyzed, b.Day)?.State == AnalysisState.Done; }, 5);
                AnalysisEntry? entry = service.Analysis(analyzed, b.Day);
                Say($"recordings: analyzed {entry?.Seconds ?? 0} s, {entry?.Labels ?? 0} labels; {service.Notice ?? "no message"}");
                Expect(done && entry is { Seconds: > 0, Labels: 1 }, $"recordings: the analysis found {entry}");
                Expect(b.AnalysisOf(analyzed) is null, "recordings: a finished analysis still shows progress");
                Expect(entry is not null && service.Notice == SoundAnalysis.Outcome(entry), $"recordings: the done message was {service.Notice ?? "missing"}");
                bool laneShown = await Until(() => b.LoudnessOf(analyzed)?.Heard == true
                    && b.Lanes.FirstOrDefault(l => l.Camera == analyzed)?.Events.Any(e => e.Kind == "smoke_alarm" && e.Source == SoundKinds.LocalAudio) == true, 5);
                Expect(laneShown, "recordings: no loudness or sound label on the lane after the analysis");
            }
            else
            {
                _problems.Add("recordings: no incident to download for the analysis");
            }

            // parity: recordings-analyze-cancel
            // Canceling an analysis before it runs: it ends canceled, shows no progress, and does not come back when
            // the core's work is done.
            RecordingCamera? other = b.Lanes.Select(l => l.Camera).FirstOrDefault(c => c != analyzed);
            if (other is { } o)
            {
                b.AnalyzeSound(o);
                Expect(b.AnalysisOf(o) is not null, "recordings: nothing to cancel");
                b.CancelAnalysis(o);
                bool canceled = await Until(() => service.Analysis(o, b.Day)?.State == AnalysisState.Canceled, 5);
                fake.CompleteRecordingWork();
                Say($"recordings: analysis canceled: {service.Notice ?? "no message"}");
                Expect(canceled && service.Analysis(o, b.Day)?.State == AnalysisState.Canceled, "recordings: the analysis did not end canceled");
                Expect(b.AnalysisOf(o) is null, "recordings: a canceled analysis still shows progress");
                Expect(service.Notice == SoundAnalysis.Outcome(new AnalysisEntry(0, AnalysisState.Canceled)), $"recordings: the cancel message was {service.Notice ?? "missing"}");
                b.ReloadLoudness(force: true);
                Expect(b.LoudnessOf(o)?.Heard != true, "recordings: a canceled analysis left loudness");
            }
            foreach (DownloadProgress d in service.Downloads.ToList())
            {
                service.Forget(d.Download);
            }
            w.Refresh();
        }

        await EditSteps(v, w, b, fake, playback, service, cams);

        // parity: recordings-layout
        // The window at its smallest and at a large size: the side panes give way in a narrow window (inspector first)
        // and come back in a wide one.
        WindowRect saved = w.CurrentFrame;
        double scale = w.Window.Content.XamlRoot?.RasterizationScale ?? 1;
        var results = new List<string>();
        (int Width, int Height)[] sizes =
        [
            ((int)Math.Ceiling(RecordingsPanes.WindowMinWidth * scale), (int)Math.Ceiling(RecordingsPanes.WindowMinHeight * scale)),
            ((int)(900 * scale), (int)(640 * scale)),
            ((int)(1400 * scale), (int)(900 * scale)),
        ];
        foreach ((int wide, int tall) in sizes)
        {
            w.ResizeTo(wide, tall);
            await Until(() => Math.Abs(w.ContentWidth - wide / scale) < 40, 5);
            await Until(() => w.ShownPanes == RecordingsPanes.Shown(w.ContentWidth, true, true) && w.ContentWidth > 0, 5);
            (bool sidebar, bool inspector) = RecordingsPanes.Shown(w.ContentWidth, true, true);
            results.Add($"{w.ContentWidth:0}: sidebar {w.ShownPanes.Sidebar}, inspector {w.ShownPanes.Inspector}");
            Expect(w.ShownPanes == (sidebar, inspector), $"recordings: at {w.ContentWidth:0} the panes are {w.ShownPanes}, expected {(sidebar, inspector)}");
        }
        Say($"recordings: layout {string.Join("; ", results)}");
        Expect(w.ContentWidth > RecordingsPanes.SidebarWidth + RecordingsPanes.InspectorWidth + RecordingsPanes.CenterMin && w.ShownPanes == (true, true), "recordings: the wide window does not show both side panes");
        w.ResizeTo((int)saved.Width, (int)saved.Height);

        // parity: recordings-window-close
        ulong hold = b.HoldId;
        ulong? openPlayback = p.Playback;
        w.Close();
        bool closed = await Until(() => w.IsClosed && RecordingsWindow.Current is null, 5);
        Expect(closed && !fake.Holds.ContainsKey(hold), "recordings: closing the window kept its hold");
        // parity: playback-close
        bool stillOpen = false;
        if (openPlayback is { } id)
        {
            try
            {
                _ = playback.PlaybackPosition(id);
                stillOpen = true;
            }
            catch (NvrView.Interop.NvException)
            {
            }
        }
        Expect(p.Playback is null && !stillOpen, "recordings: closing the window left its playback open");
        Say($"recordings: closed; hold released {!fake.Holds.ContainsKey(hold)}, playback closed {!stillOpen}");
        thumbnailQueue.Dispose();
    }


    // A cover keyframe through the real decoder (Media Foundation and WIC): the size, the bars' colors, and a row that shows it.
    private async Task RecordingsThumbnails(RecordingsService service, FakeRecordings fake, RecordingsBrowser b, RecordingsWindow w)
    {
        string? exe = options.MockNvr;
        string? root = exe is null ? null : Path.GetDirectoryName(Path.GetDirectoryName(Path.GetDirectoryName(exe)));
        string fixtures = root is null ? "" : Path.Combine(root, "fixtures", "synthetic");
        if (!Directory.Exists(fixtures))
        {
            _problems.Add("recordings: no synthetic fixtures to decode a cover keyframe from");
            return;
        }
        byte[] ReadKeyframe(string file, bool h265) => AnnexBKeyframes.Last(File.ReadAllBytes(Path.Combine(fixtures, file)), h265);
        // Each codec's colors fixture at its own size, and the resize fixture's last keyframe (480 x 270) shrunk to 320 wide.
        foreach ((string codec, bool h265, NvThumbKind kind) in new[] { ("h264", false, NvThumbKind.H264Keyframe), ("h265", true, NvThumbKind.H265Keyframe) })
        {
            byte[] colors = ReadKeyframe($"{codec}-colors.{codec}", h265);
            Nv12Picture? raw = KeyframeImage.Decode(h265 ? NvrView.Interop.NvCodec.H265 : NvrView.Interop.NvCodec.H264, colors, 192, 96, out _);
            (YuvMatrix? vuiMatrix, bool? vuiFull) = h265 ? HevcColor.Read(colors) : (null, null);
            Say($"recordings: {codec} decoder {raw?.Decoder}, matrix {raw?.Matrix}, full range {raw?.FullRange}; stream says {vuiMatrix?.ToString() ?? "nothing"}, {vuiFull?.ToString() ?? "nothing"}");
            RecordedThumbnail? small = KeyframeImage.Thumbnail(new RecordedThumbnail(kind, 192, 96, 0, colors), out string? why);
            if (small is null)
            {
                // The H.265 decoder comes with the HEVC Video Extension; without it that codec is reported, not failed.
                if (h265 && why?.Contains("no H.265 decoder", StringComparison.Ordinal) == true)
                {
                    Say($"recordings: {codec} cover keyframe skipped ({why})");
                    continue;
                }
                _problems.Add($"recordings: the {codec} cover keyframe did not decode: {why}");
                continue;
            }
            var image = await DecodeJpeg(small.Data);
            if (image is not { } decoded || decoded.Width != 192 || decoded.Height != 96 || small.Width != 192 || small.Height != 96)
            {
                _problems.Add($"recordings: the {codec} cover is {image?.Width}x{image?.Height}, expected 192x96");
                continue;
            }
            int worst = 0;
            for (int i = 0; i < ColorBars.Length; i++)
            {
                int o = (24 * decoded.Width + 16 + 32 * i) * 4;
                (int r, int g, int bl) = (decoded.Pixels[o + 2], decoded.Pixels[o + 1], decoded.Pixels[o]);
                worst = Math.Max(worst, Math.Max(Math.Abs(r - ColorBars[i].R), Math.Max(Math.Abs(g - ColorBars[i].G), Math.Abs(bl - ColorBars[i].B))));
            }
            Say($"recordings: {codec} cover {decoded.Width}x{decoded.Height}, {small.Data.Length} bytes, bars within {worst} codes");
            Expect(worst <= 14, $"recordings: the {codec} cover's bar colors are {worst} codes off");

            byte[] resize = ReadKeyframe($"{codec}-resize.{codec}", h265);
            RecordedThumbnail? shrunk = KeyframeImage.Thumbnail(new RecordedThumbnail(kind, 480, 270, 0, resize), out why);
            var shrunkImage = shrunk is null ? null : await DecodeJpeg(shrunk.Data);
            Say($"recordings: {codec} 480x270 keyframe shrunk to {shrunkImage?.Width}x{shrunkImage?.Height}");
            Expect(shrunkImage is { Width: 320, Height: 180 }, $"recordings: the {codec} 480x270 cover is {shrunkImage?.Width}x{shrunkImage?.Height} ({why})");
        }

        // The window: the first incident's covers are keyframes in the core's cache; its rows decode and show them,
        // and the small pictures are put back.
        Incident? incident = b.DayIncidents.OrderBy(i => i.Start).FirstOrDefault();
        if (incident is null)
        {
            _problems.Add("recordings: no incident for a cover");
            return;
        }
        byte[] keyframe = ReadKeyframe("h264-colors.h264", false);
        foreach (ThumbnailKey key in incident.CoverKeys)
        {
            fake.ThumbnailPut(key.Json, new RecordedThumbnail(NvThumbKind.H264Keyframe, 192, 96, 0, keyframe));
        }
        int putBefore = service.ThumbnailsPut;
        int shownBefore = w.ThumbnailsShown;
        service.CancelThumbnails();
        w.Refresh();
        bool shown = await Until(() => w.ThumbnailsShown > shownBefore && service.Thumbnails[incident.CoverKeys[0]] is ThumbnailState.Image, 10);
        ThumbnailKey first = incident.CoverKeys[0];
        RecordedThumbnail? cached = fake.ThumbnailGet(first.Json);
        Say($"recordings: row covers shown {w.ThumbnailsShown - shownBefore}, put back {service.ThumbnailsPut - putBefore}, cached kind {cached?.Kind}");
        Expect(shown && service.ThumbnailsPut > putBefore && cached?.Kind == NvThumbKind.Image,
            "recordings: the incident's row did not show the decoded cover, or it was not put back in the core's cache");
    }

    // 11e. The follow cut and grids of a downloaded incident (docs/UX.md "Recordings", "Follow and Grid"; milestone P,
    // W3), on the fake store's plans over synthetic files written where its download says they are: each camera's
    // pictures are its own color with the frame number in a strip (SyntheticRecording). Frames are drawn offscreen with
    // D3D11 from Media Foundation-decoded files (a second set of the plan's files, so the window's render thread cannot
    // move the pictures meanwhile) and checked at sample times for which camera is shown and which of its frames; then
    // the cuts, the sound following the shown camera, the transport on a simulated clock, the picture on screen, the
    // window's view menu, and the way back to the cameras.
    private async Task EditSteps(
        Viewer v, RecordingsWindow w, RecordingsBrowser b, FakeRecordings fake, FakePlayback playback, RecordingsService service, RecordingCamera[] cams)
    {
        Task<bool> Playing(Func<bool> condition, double seconds) => Until(() =>
        {
            playback.CompletePlaybackWork();
            return condition();
        }, seconds);
        RecordingsPlayer p = b.Player;
        EditPlayer ep = b.EditPlayer;
        Func<double> realClock = ep.Now;

        // parity: edit-download
        Incident? incident = b.DayIncidents.FirstOrDefault(i => i.Kinds.SequenceEqual(["vehicle"]) && i.Cameras[0].Camera == cams[0]);
        if (incident is null)
        {
            _problems.Add("edits: no vehicle incident of the first camera");
            return;
        }
        long s = incident.Start * 1000;
        b.Selection = new HashSet<string> { incident.Key };
        await Playing(() => b.Selected?.Key == incident.Key && p.Playback is not null, 5);
        w.Refresh();
        (string title, List<PlayerView> offered, bool hint) = w.ViewMenuShown();
        Expect(b.Editable is null && offered.SequenceEqual([PlayerView.Cameras]) && hint && title == "Cameras",
            $"edits: an incident not downloaded offers {string.Join(", ", offered)} (hint {hint})");
        b.ShowView(PlayerView.Follow);
        Expect(b.PlayerView == PlayerView.Cameras, "edits: Follow Cut showed without a download");
        b.Message = null;
        // The fake store has one camera per incident; its download of three cameras with video along with this one's stands for an edited incident.
        string request = $$"""{"incident":"{{incident.Key}}","cameras":[{{string.Join(",", cams.Select(c => $$"""{"camera":{{c.Json}}}"""))}}]}""";
        fake.DownloadIncident(request);
        bool downloaded = await Until(() =>
        {
            fake.CompleteRecordingWork();
            return service.FinishedDownload(incident.Key) is not null;
        }, 5);
        DownloadProgress? d = service.FinishedDownload(incident.Key);
        var files = new List<(string File, int Source, long DurationMs)>();
        if (downloaded && d is not null)
        {
            using var raw = System.Text.Json.JsonDocument.Parse(fake.EditCompute($$"""{"kind":"follow","download":"{{d.Download}}"}"""));
            foreach (System.Text.Json.JsonElement src in raw.RootElement.GetProperty("sources").EnumerateArray())
            {
                files.Add((src.GetProperty("file").GetString()!, src.GetProperty("source").GetInt32(), src.GetProperty("duration_ms").GetInt64()));
            }
        }
        string? writeFailure = null;
        try
        {
            await Task.Run(() =>
            {
                foreach ((string file, int source, long ms) in files)
                {
                    SyntheticRecording.Write(new SyntheticRecording.Spec(Frames: (int)(ms / 100) + 1, Color: source), file);
                }
            });
        }
        catch (Exception e) when (e is VideoException or IOException)
        {
            writeFailure = e.Message;
        }
        w.Refresh();
        (title, offered, hint) = w.ViewMenuShown();
        Say($"edits: downloaded {d?.Download}, {files.Count} synthetic files written{(writeFailure is null ? "" : ", failed: " + writeFailure)}; the view menu offers {string.Join(", ", offered)}");
        Expect(writeFailure is null, $"edits: the synthetic files were not written ({writeFailure})");
        Expect(downloaded && files.Count == 3, $"edits: the download has {files.Count} files, expected 3");
        Expect(b.Editable is not null && offered.Count == 4 && !hint, "edits: a finished download does not offer the follow cut and grids");
        if (!downloaded || d is null || files.Count != 3 || writeFailure is not null)
        {
            return;
        }

        // parity: edit-follow-cut
        double now = 1000;
        ep.Now = () => now;
        b.ShowView(PlayerView.Follow);
        bool opened = await Until(() => ep.Files is not null && (w.EditShown?.Frames ?? 0) > 0, 10);
        if (!opened || ep.Plan is not { } plan || ep.Files is not EditFiles live)
        {
            _problems.Add($"edits: the follow cut did not open ({ep.Message ?? b.Message ?? "no message"})");
            ep.Now = realClock;
            b.ShowView(PlayerView.Cameras);
            return;
        }
        Say($"edits: follow cut {string.Join(", ", plan.Segments.Select(g => $"{g.Source}@{g.StartMs - s}"))}, cuts {string.Join(", ", ep.Cuts.Select(c => c - s))}, frames drawn {w.EditShown?.Frames}");
        // The device playback's pause arrives as a core event, and the edit
        // can draw its first frame before it.
        await Until(() => !p.Transport.IsPlaying, 5);
        Expect(b.PlayerView == PlayerView.Follow && !p.Transport.IsPlaying, "edits: the device playback still plays under the follow cut");
        Expect(live.Failures.Count == 0, $"edits: files not opened: {string.Join("; ", live.Failures.Select(f => $"{f.Key}: {f.Value}"))}");
        Expect(ep.Cuts.SequenceEqual([s + 1000, s + 2000]) && plan.Segments.Select(g => g.Source).SequenceEqual([0, 1, 2]),
            $"edits: the follow cut cuts at {string.Join(",", ep.Cuts.Select(c => c - s))} to {string.Join(",", plan.Segments.Select(g => g.Source))}");
        w.Refresh();
        Expect(w.Scrubber.CutTicks.SequenceEqual(ep.Cuts) && w.ViewMenuShown().Title == "Follow Cut", "edits: the scrubber's cut marks or the view menu's title are not those of the follow cut");
        Expect(w.LabelTexts().Any(t => t.StartsWith("Camera 1", StringComparison.Ordinal)), $"edits: the follow cut's label is {string.Join("; ", w.LabelTexts())}");

        // Frames of every camera at sample times, drawn offscreen from files of their own.
        using (var probe = EditFiles.Open(plan, () => v.Host.Gpu))
        using (var renderer = new GridRenderer(v.Host.Gpu, 320, 180))
        {
            const long n = 24;
            long[] samples =
            [
                .. Enumerable.Range(0, (int)n).Select(k => plan.StartMs + k * (plan.EndMs - plan.StartMs) / (n - 1)),
                s + 500, s + 999, s + 1000, s + 1500, s + 1999, s + 2000, s + 2100, s + 700,
            ];
            List<string> bad = EditMismatches(probe, plan, renderer, samples);
            Say($"edits: follow cut rendered {samples.Length} frames, {bad.Count} wrong");
            Expect(bad.Count == 0, $"edits: follow cut pictures: {string.Join("; ", bad.Take(4))}");
        }

        // The transport on the simulated clock, and the sound following the picture.
        ep.Pause();
        ep.SetRate(1);
        ep.Seek(s + 500);
        ep.Play();
        var heard = new List<int?> { ep.PlayingSoundSource };
        now += 1;
        ep.Tick();
        heard.Add(ep.PlayingSoundSource);
        now += 1;
        ep.Tick();
        heard.Add(ep.PlayingSoundSource);
        long position = ep.PositionMs;
        ep.SetRate(4);
        heard.Add(ep.PlayingSoundSource);
        bool keyframes = ep.KeyframesOnly;
        ep.SetRate(1);
        ep.Muted = true;
        heard.Add(ep.PlayingSoundSource);
        ep.Muted = false;
        ep.Pause();
        string heardText = string.Join(", ", heard.Select(h => h?.ToString() ?? "none"));
        Say($"edits: played 2 s on the simulated clock to {position - s} ms; sound from {heardText}");
        Expect(position == s + 2500, $"edits: 2 s of play moved to {position - s} ms, expected 2500");
        Expect(heardText == "0, 1, 2, none, none" && keyframes, $"edits: the sound came from {heardText} (keyframes at 4x {keyframes}), expected 0, 1, 2, none, none");

        // On screen: the third camera's picture at 2.5 s, from the window's own renderer.
        bool shown = await Until(() => live.File(2)?.ShownPositionMs == 5500, 5);
        if (w.Renderer is { } stage)
        {
            StageCapture capture = await stage.CaptureAsync();
            (int r, int g, int bl) = capture.Image.Rgb(capture.Image.Width / 2, capture.Image.Height / 2);
            (int R, int G, int B) want = SyntheticRecording.Palette[2];
            Say($"edits: on screen ({r}, {g}, {bl}), expected {want}; the third camera's file at {live.File(2)?.ShownPositionMs} ms");
            Expect(shown && Math.Abs(r - want.R) <= 40 && Math.Abs(g - want.G) <= 40 && Math.Abs(bl - want.B) <= 40,
                $"edits: the screen shows ({r}, {g}, {bl}) at 2.5 s, expected the third camera's {want}");
        }

        // A lane click inside the edit's range on one of its cameras seeks the edit (and plays); another place brings the cameras back.
        b.LaneClick(cams[1], incident.Start + 4);
        Expect(b.PlayerView == PlayerView.Follow && ep.PositionMs == s + 4000, $"edits: a lane click in the range went to {ep.PositionMs - s} ms in {b.PlayerView}");
        ep.Pause();

        // parity: edit-grid
        b.ShowView(PlayerView.Grid4);
        bool gridOpened = await Until(() => ep.Files is not null && ep.Plan?.Kind == EditKind.Grid && (w.EditShown?.Frames ?? 0) > 0, 10);
        if (!gridOpened || ep.Plan is not { } grid)
        {
            _problems.Add($"edits: the grid did not open ({ep.Message ?? "no message"})");
        }
        else
        {
            Say($"edits: grid {grid.Layout} of {grid.Cells.Count} cells on {grid.Canvas?.Width}x{grid.Canvas?.Height}");
            Expect(grid.Layout == "4up" && grid.Cells.Count == 3 && grid.Canvas == new EditCanvas(1920, 1080),
                $"edits: the 4-up grid is {grid.Layout} with {grid.Cells.Count} cells");
            using (var probe = EditFiles.Open(grid, () => v.Host.Gpu))
            using (var renderer = new GridRenderer(v.Host.Gpu, 640, 360))
            {
                const long n = 16;
                long[] samples =
                [
                    .. Enumerable.Range(0, (int)n).Select(k => grid.StartMs + k * (grid.EndMs - grid.StartMs) / (n - 1)),
                    grid.StartMs + 500, grid.StartMs + 1500,
                ];
                List<string> bad = EditMismatches(probe, grid, renderer, samples);
                Say($"edits: grid rendered {samples.Length} frames, {bad.Count} wrong");
                Expect(bad.Count == 0, $"edits: grid pictures: {string.Join("; ", bad.Take(4))}");
            }
            w.Refresh();
            Expect(w.EditCells.Count == 3 && w.LabelTexts().Count(t => t.StartsWith("Camera ", StringComparison.Ordinal)) >= 3,
                $"edits: the grid's cells or labels are {w.EditCells.Count}, {string.Join("; ", w.LabelTexts())}");
            ep.Seek(grid.StartMs);
            w.Refresh();
            int absent = w.MessageTexts().Count(m => m == "No recording at this time");
            Expect(absent == 2, $"edits: {absent} cells say there is no recording at the start, expected the two later cameras'");
            ep.Seek(grid.StartMs + 6000);
            ep.Play();
            int? first = ep.PlayingSoundSource;
            ep.GridSound = 1;
            int? second = ep.PlayingSoundSource;
            ep.Pause();
            ep.GridSound = 0;
            Expect(first == 0 && second == 1, $"edits: the grid's sound came from {first?.ToString() ?? "none"}, then {second?.ToString() ?? "none"}");
        }

        // parity: edit-back-to-cameras
        long at = ep.PositionMs;
        ep.Now = realClock;
        b.ShowView(PlayerView.Cameras);
        w.Refresh();
        Say($"edits: back to the cameras at {TransportState.Clock(p.PositionMs)}");
        Expect(b.PlayerView == PlayerView.Cameras && ep.Files is null && w.EditShown is null, "edits: the edit did not close");
        if (p.Playback is not null)
        {
            Expect(p.PositionMs == at, $"edits: the cameras continue at {p.PositionMs}, not the edit's {at}");
        }

        // The edit is closed, so its files can be written again: with sound for the export steps, then as they were.
        await ExportEditSteps(w, b, fake, d.Download, s, files);
        try
        {
            if (d.Folder is { } folder)
            {
                Directory.Delete(folder, recursive: true);
            }
        }
        catch (IOException)
        {
        }
    }

    // Renders the samples of a plan offscreen from a second set of its files and returns what did not match.
    private static List<string> EditMismatches(EditFiles files, EditPlan plan, GridRenderer renderer, long[] samples)
    {
        var bounds = new NvrView.Shell.Rect(0, 0, renderer.Width, renderer.Height);
        var bad = new List<string>();
        foreach (long t in samples)
        {
            RenderedImage image = files.Render(t, renderer);
            if (plan.Kind == EditKind.Follow)
            {
                if (EditTimeline.Shot(plan, t) is not { } shot)
                {
                    continue;
                }
                (int? color, int frame) = SyntheticRecording.Read(image, bounds);
                if (color != shot.Source || frame != shot.FilePositionMs / 100)
                {
                    bad.Add($"{t - plan.StartMs} ms: camera {color?.ToString() ?? "none"} frame {frame}, expected {shot.Source} frame {shot.FilePositionMs / 100}");
                }
                continue;
            }
            List<NvrView.Shell.Rect> rects = EditComposition.CellRects(plan, bounds);
            for (int i = 0; i < 4; i++)
            {
                var cell = i < rects.Count ? rects[i] : new NvrView.Shell.Rect(bounds.Width / 2, bounds.Height / 2, bounds.Width / 2, bounds.Height / 2);
                if (i < plan.Cells.Count && EditTimeline.CellPosition(plan, i, t) is { } position)
                {
                    NvrView.Shell.Rect picture = PictureFit.Of(640, 360, cell, fill: false).Destination;
                    (int? color, int frame) = SyntheticRecording.Read(image, picture);
                    if (color != plan.Cells[i].Source || frame != position / 100)
                    {
                        bad.Add($"{t - plan.StartMs} ms cell {i}: camera {color?.ToString() ?? "none"} frame {frame}, expected {plan.Cells[i].Source} frame {position / 100}");
                    }
                }
                else
                {
                    (int r, int g, int bl) = image.Rgb((int)cell.MidX, (int)cell.MidY);
                    if (Math.Max(r, Math.Max(g, bl)) > 40)
                    {
                        bad.Add($"{t - plan.StartMs} ms cell {i}: ({r}, {g}, {bl}), expected black");
                    }
                }
            }
        }
        return bad;
    }
}
