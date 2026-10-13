// The pacing steps of the self-test (docs/RENDERING.md "Playback pacing" and "Live pacing"): the tiles' pacing traces
// record when each picture reached the app, was decoded, and was first drawn. The macOS self-test measures the same
// (the shared step IDs are live-pacing and playback-pacing).
//
// Where each part is proven:
// - that the core releases pictures at their own pace (live pictures a device sends in pairs, a recording whose device
//   clock is in another zone): the core's tests on a simulated clock (crates/viewer-core/tests/core.rs
//   live_pictures_delivered_in_groups_are_decoded_at_their_own_pace, tests/playback.rs
//   pictures_are_released_evenly_when_the_given_utc_offset_is_not_the_devices);
// - the display stage and the bounds, including the old bugs injected (decode on arrival, bursts from a wrong UTC
//   offset): PacingRefresh and PacingVerdict in NvrView.Shell, with the tests in NvrView.Shell.Tests/PacingTests.cs;
// - here, end to end in the running app: the real core against the mock, the real decoder, and the real renderer. The
//   verdict is taken on the decode times shown by an ideal 60 Hz display (PacingRefresh), so it does not depend on
//   how many refreshes the real display gave a window behind the others; the real display's share of drawn pictures
//   is checked as wiring. The steps wait for picture counts, never for a fixed time.

using NvrView.Shell;
using NvrView.Video;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    /// <summary>Share of the released pictures the real display must draw (wiring: the renderer takes new pictures).</summary>
    internal const double RealShownRatio = 0.8;

    /// <summary>Live pictures each tile must deliver before the live verdict.</summary>
    internal const int LivePictures = 30;

    /// <summary>Shown picture pairs the playback verdict needs (about two seconds at the self-test mock's 20 fps).</summary>
    internal const int PlaybackPairs = 40;

    /// <summary>
    /// Live pacing: every live tile of the first window traces its pictures until each delivered
    /// <see cref="LivePictures"/>; the pictures the mock sends in pairs must be shown at their own pace.
    /// </summary>
    private async Task LivePacingStep(StageRenderer r)
    {
        // parity: live-pacing
        (VideoTile Video, PacingTrace Trace)[] traces = [.. Tiles.Where(t => t.Video is not null).Select(t => (t.Video!, new PacingTrace()))];
        foreach ((VideoTile video, PacingTrace trace) in traces)
        {
            video.Pacing = trace;
        }
        long frames = r.Frames;
        var watch = System.Diagnostics.Stopwatch.StartNew();
        bool enough = await Until(() => traces.All(t => t.Trace.Counts.Decoded >= LivePictures), 15);
        foreach ((VideoTile video, _) in traces)
        {
            video.Pacing = null;
        }
        PacingReport[] reports = [.. traces.Select(t => t.Trace.Report())];
        int delivered = reports.Sum(x => x.Released.Pictures), shown = reports.Sum(x => x.Shown.Pictures);
        int simulated = reports.Sum(x => x.Simulated.Pictures);
        double ratio = delivered > 0 ? (double)shown / delivered : 0, simRatio = delivered > 0 ? (double)simulated / delivered : 0;
        Say($"live pacing: {traces.Length} tiles, {delivered} pictures delivered, {shown} drawn ({ratio:0.00}), {simulated} on the simulated display ({simRatio:0.00}), " +
            $"{r.Frames - frames} frames in {watch.Elapsed.TotalSeconds:0.0} s; first tile: {(reports.Length > 0 ? reports[0].ToString() : "none")}");
        Expect(enough, $"live pacing: the tiles did not each deliver {LivePictures} pictures");
        Expect(simRatio >= PacingVerdict.ShownRatio, $"live pacing: {simulated} of {delivered} delivered pictures would be shown at 60 Hz");
        Expect(ratio >= RealShownRatio, $"live pacing: {shown} of {delivered} delivered pictures were drawn");
    }

    private async Task PlaybackPacingStep(Viewer v)
    {
        // parity: playback-pacing
        if (app.Recordings is not { Available: true } service || v.Devices.FirstOrDefault() is not { State: "connected" } device)
        {
            _problems.Add("playback pacing: no recordings service or mock device");
            return;
        }
        var camera = new RecordingCamera(device.Key, 0);
        service.SetEnabled(camera, true);
        RecordingsWindow w = RecordingsWindow.Open(v, v.Host, service, background: true);
        await Until(() => w.IsLoaded, 10);
        RecordingsBrowser b = w.Browser;
        RecordingsPlayer p = b.Player;
        // The owner's path: an incident selected on the day's timeline opens a playback from its start less its
        // margin (the Incident scrubber), here a finished one of the one enabled camera.
        // Once the day is searched it may hold only a growing incident (the mock records days by UTC date, so early in
        // its day, or just after midnight east of UTC, its newest day has no finished incident yet): then yesterday.
        await Until(() => !service.Searching.Busy && b.DayIncidents.Count > 0, 30);
        bool listed = b.DayIncidents.Any(i => !i.Growing);
        if (!listed)
        {
            b.PreviousDay();
            listed = await Until(() => b.DayIncidents.Any(i => !i.Growing) && !service.Searching.Busy, 30);
        }
        Incident? incident = b.DayIncidents.Where(i => !i.Growing).OrderBy(i => i.Start).FirstOrDefault();
        if (!listed || incident is null)
        {
            _problems.Add($"playback pacing: the mock's day listed no finished incident ({b.DayIncidents.Count} incidents; day {b.Day}, " +
                $"holding {b.Holding}, searching {service.Searching}, lanes {string.Join(", ", b.Lanes.Select(l => $"{l.Camera.Device}/{l.Camera.Channel}: {l.Spans.Count} spans, {l.Events.Count} events, failed {l.Failed}"))}; " +
                $"enabled {string.Join(", ", service.EnabledCameras)})");
            w.Close();
            await Until(() => w.IsClosed, 5);
            return;
        }
        b.Click(incident.Key, extend: false);
        Say($"playback pacing: incident {incident.Start}..{incident.End} (margin {incident.Margin} s, {incident.Cameras.Count} camera(s)), player at {p.PositionMs / 1000}");
        VideoTile? video = null;
        var trace = new PacingTrace();
        bool started = await Until(() =>
        {
            if (video is null && p.Cells.Count == 1 && v.Host.VideoOf(p.Cells[0].Tile) is { } found)
            {
                video = found;
                video.Pacing = trace;
            }
            return p.State == NvrView.Interop.NvPlaybackState.Playing && trace.Counts.Shown >= 10;
        }, 30);
        if (!started || video is null)
        {
            _problems.Add($"playback pacing: the playback did not start (state {p.State}, cells {p.Cells.Count}, message {p.Message ?? "none"})");
            w.Close();
            await Until(() => w.IsClosed, 5);
            return;
        }
        VideoTile tile = video;
        // Past the play's opening burst (ten more pictures), then measured until enough pictures were decoded.
        trace.Reset();
        await Until(() => trace.Counts.Decoded >= 10, 10);
        trace.Reset();
        long positionBefore = p.PositionMs;
        var watch = System.Diagnostics.Stopwatch.StartNew();
        bool enough = await Until(() => trace.Counts.Decoded >= PlaybackPairs + 5, 20);
        PacingReport report = trace.Report(PlaybackPairs);
        tile.Pacing = null;
        Say($"playback pacing (incident at 1x): decoder {tile.DecoderName} ({tile.DecoderKind}), " +
            $"position moved {p.PositionMs - positionBefore} ms in {watch.Elapsed.TotalSeconds:0.0} s, drawn/released {report.ShownRatio:0.00}; {report}");
        Expect(enough, $"playback pacing: only {trace.Counts.Decoded} pictures were decoded in 20 s");
        foreach (string problem in report.Problems)
        {
            _problems.Add($"playback pacing (simulated 60 Hz display): {problem}");
        }
        Expect(report.ShownRatio >= RealShownRatio, $"playback pacing: the display drew {report.Shown.Pictures} of {report.Released.Pictures} released pictures");
        // The same playback, after the verdict: the player's picture features (zoom, pan, Fill, Save Picture).
        await PlaybackZoomStep(v, w, p);
        await PlaybackRefusedStep(p);
        w.Close();
        await Until(() => w.IsClosed, 5);
        service.SetEnabled(camera, false);
    }

    /// <summary>
    /// A play the device refuses (FFI-RECORDINGS.md, <c>status</c> on a failed camera): the mock answers every play at
    /// 16x with status 400 (<c>--refuse-play-speed 16</c>, App.xaml.cs), so at 16x the player's camera says the NVR
    /// refused it, with the status, instead of showing an empty tile; back at 1x it plays again.
    /// </summary>
    private async Task PlaybackRefusedStep(RecordingsPlayer p)
    {
        // parity: playback-refused
        const string refused = "The NVR refused this playback (status 400)";
        p.Play();
        p.SetRate(16);
        bool shown = await Until(() => p.Cells.Count > 0 && p.Cells.All(c => RecordingsWindow.CellMessage(c) == refused), 30);
        string seen = string.Join(", ", p.Cells.Select(c => RecordingsWindow.CellMessage(c) ?? $"{c.State} {c.Reason}"));
        Say($"playback refused: at 16x {seen}");
        Expect(shown, $"playback refused: the cameras show {seen}, expected \"{refused}\"");
        p.SetRate(1);
        bool again = await Until(() => p.Cells.All(c => c.State is "playing" or "buffering"), 30);
        Say($"playback refused: back at 1x {string.Join(", ", p.Cells.Select(c => c.State))}");
        Expect(again, "playback refused: the cameras did not play again at 1x");
        p.Pause();
    }
}
