// The playback pacing step of the self-test (docs/RENDERING.md "Playback pacing"): one camera of the mock's
// recordings plays at 1x in a Recordings window on the real core, while the first window keeps its live tiles, and
// the tile's pacing trace measures when each picture reached the app, was decoded, and was first drawn. The shown
// pictures' intervals must follow their own timestamps within a bound. The macOS self-test measures the same
// (the shared step ID is playback-pacing).
//
// This step waits on real time on purpose: what it measures is real pacing (the engine's release clock, the
// decoder, the display refresh), which a simulated clock cannot show. Ten seconds give about a hundred pictures at
// the mock's 10 fps.

using NvrView.Shell;
using NvrView.Video;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    /// <summary>Jitter (ms) the shown pictures may have at the 95th percentile: one 60 Hz refresh plus a few ms.</summary>
    internal const double PacingJitterP95Ms = 20;

    /// <summary>Largest jitter (ms) allowed for any shown picture: two refreshes.</summary>
    internal const double PacingJitterMaxMs = 34;

    /// <summary>Share of the released pictures that must be drawn.</summary>
    internal const double PacingShownRatio = 0.9;

    private static double ShownRatio(PacingReport r) => r.Released.Pictures > 0 ? (double)r.Shown.Pictures / r.Released.Pictures : 0;

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
        bool listed = await Until(() => b.DayIncidents.Any(i => !i.Growing) && !service.Searching.Busy, 30);
        Incident? incident = b.DayIncidents.Where(i => !i.Growing).OrderBy(i => i.Start).FirstOrDefault();
        if (!listed || incident is null)
        {
            _problems.Add($"playback pacing: the mock's day listed no finished incident ({b.DayIncidents.Count} incidents)");
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
            return p.State == NvrView.Interop.NvPlaybackState.Playing && trace.Report().Shown.Pictures >= 10;
        }, 30);
        if (!started || video is null)
        {
            _problems.Add($"playback pacing: the playback did not start (state {p.State}, cells {p.Cells.Count}, message {p.Message ?? "none"})");
            w.Close();
            await Until(() => w.IsClosed, 5);
            return;
        }
        VideoTile tile = video;
        // Past the play's opening burst, then ten seconds measured.
        await Task.Delay(2000);
        async Task<PacingReport> Measure(string label)
        {
            trace.Reset();
            long positionBefore = p.PositionMs;
            await Task.Delay(10_000);
            PacingReport r = trace.Report();
            Say($"playback pacing ({label}): decoder {tile.DecoderName} ({tile.DecoderKind}), " +
                $"position moved {p.PositionMs - positionBefore} ms in 10 s, shown/released {ShownRatio(r):0.00}; {r}");
            return r;
        }
        PacingReport report = await Measure("incident at 1x");
        tile.Pacing = null;
        Expect(report.Shown.Pairs >= 50, $"playback pacing: only {report.Shown.Pairs} picture pairs were shown in 10 s");
        Expect(report.Shown.JitterP95 <= PacingJitterP95Ms && report.Shown.JitterMax <= PacingJitterMaxMs,
            $"playback pacing: shown pictures are uneven (jitter p95 {report.Shown.JitterP95:0.0} ms, max {report.Shown.JitterMax:0.0} ms; " +
            $"bounds {PacingJitterP95Ms} and {PacingJitterMaxMs} ms)");
        Expect(ShownRatio(report) >= PacingShownRatio, $"playback pacing: {report.Shown.Pictures} of {report.Released.Pictures} released pictures were shown");
        Expect(report.Skipped == 0, $"playback pacing: {report.Skipped} decoded pictures were never shown");
        w.Close();
        await Until(() => w.IsClosed, 5);
        service.SetEnabled(camera, false);
    }
}
