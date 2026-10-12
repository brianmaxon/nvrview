using System.Text.Json;
using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Shell.Tests;

/// <summary>
/// The recordings' pure side (docs/CAPTURE.md): the settings mapping, the pills, the messages for each
/// limit and each end, the board of running recordings, the call recording's shared stem and its
/// pruning. The same vectors as the macOS app's CaptureRecordingsTests.
/// </summary>
internal static class CaptureRecordingsTests
{
    private static RecordingEvent Event(ulong id, string state, string? reason, ulong ms, string kind = "live") =>
        new(id, kind, 1, 2, 0, "main", state, reason, "/x/a.mp4", 1, 0, ms, 0, "none", "video", 0, null, null);

    private static JsonElement Object(string json) => JsonDocument.Parse(json).RootElement;

    public static void Run()
    {
        T.Run("recordings: the settings map to the configure JSON", () =>
        {
            var s = new CaptureSettings("C:\\Users\\example\\Videos\\nvrview");
            JsonElement o = Object(s.Json());
            T.Equal("C:\\Users\\example\\Videos\\nvrview", o.GetProperty("live_dir").GetString());
            T.Equal("main", o.GetProperty("quality").GetString());
            T.Check(!o.GetProperty("share_safe").GetBoolean());
            T.Equal(3600, o.GetProperty("max_duration_s").GetInt32());
            T.Equal(4_000_000_000UL, o.GetProperty("max_bytes").GetUInt64());
            T.Equal(2_000_000_000UL, o.GetProperty("min_free_bytes").GetUInt64());
            T.Equal(1_000_000_000UL, o.GetProperty("stop_free_bytes").GetUInt64());
            T.Equal(4, o.GetProperty("max_live").GetInt32());
            T.Equal(2, o.GetProperty("max_calls").GetInt32());

            o = Object((s with { Quality = RecordQuality.Sub, ShareSafe = true, Minutes = 0, Bytes = 1 }).Json());
            T.Equal("sub", o.GetProperty("quality").GetString());
            T.Check(o.GetProperty("share_safe").GetBoolean());
            // Clamped to 1 minute and 100 MB.
            T.Equal(60, o.GetProperty("max_duration_s").GetInt32());
            T.Equal(100_000_000UL, o.GetProperty("max_bytes").GetUInt64());
            o = Object((s with { Minutes = 100_000, Bytes = ulong.MaxValue }).Json());
            T.Equal(12 * 3600, o.GetProperty("max_duration_s").GetInt32());
            T.Equal(64_000_000_000UL, o.GetProperty("max_bytes").GetUInt64());
        });

        T.Run("recordings: the pills", () =>
        {
            T.Equal("● REC 1:23", RecordingText.TilePill(83_400));
            T.Equal("● Recording 0:42", RecordingText.CallPill(42_000));
            T.Equal("1:02:03", RecordingText.Elapsed(3_723_000));
            T.Equal("0:00", RecordingText.Elapsed(0));
            T.Equal("4 GB", RecordingText.Size(4_000_000_000));
            T.Equal("1.5 GB", RecordingText.Size(1_500_000_000));
            T.Equal("500 MB", RecordingText.Size(500_000_000));
            T.Equal("60 minutes", RecordingText.Minutes(60));
            T.Equal("1 minute", RecordingText.Minutes(1));
            T.Equal("2 hours", RecordingText.Minutes(120));
            T.Equal("90 minutes", RecordingText.Minutes(90));
        });

        T.Run("recordings: a refused start names the limit", () =>
        {
            var s = new CaptureSettings("C:\\x");
            (string Text, bool Offer) Refused(string limit) => RecordingText.Refused(NvStatus.Limit, limit, s, call: false);
            T.Check(Refused("live").Text.Contains("4 cameras", StringComparison.Ordinal) && !Refused("live").Offer);
            T.Check(Refused("calls").Text.Contains("2 calls", StringComparison.Ordinal));
            T.Check(Refused("main_streams").Text.Contains("two main streams", StringComparison.Ordinal) && Refused("main_streams").Offer);
            T.Check(Refused("disk_low").Text.Contains("free disk space", StringComparison.Ordinal) && Refused("disk_low").Text.Contains("2 GB", StringComparison.Ordinal));
            T.Equal("Recording is at a limit and cannot start.", RecordingText.Refused(NvStatus.Limit, null, s, false).Text);
            T.Check(RecordingText.Refused(NvStatus.NotConfigured, null, s, call: true).Text.Contains("encoder", StringComparison.Ordinal));
            T.Equal("Recording is not set up.", RecordingText.Refused(NvStatus.NotConfigured, null, s, call: false).Text);
            T.Check(RecordingText.Refused(NvStatus.UnknownId, null, s, false).Text.Contains("not open", StringComparison.Ordinal));
            T.Check(RecordingText.Refused(NvStatus.Closed, null, s, false).Text.Contains("Closed", StringComparison.Ordinal));
        });

        T.Run("recordings: what an end says", () =>
        {
            var s = new CaptureSettings("C:\\x") { Minutes = 120 };
            T.Equal("Recording saved", RecordingText.Ended(Event(1, "finished", "stopped", 0), s));
            T.Equal("Call recording saved", RecordingText.Ended(Event(1, "finished", "stopped", 0, "call"), s));
            T.Equal("Recording failed", RecordingText.Ended(Event(1, "failed", "stopped", 0), s));
            T.Equal("Recording stopped at its 2 hours limit", RecordingText.Ended(Event(1, "finished", "max_duration", 0), s));
            T.Equal("Recording stopped at its 4 GB limit", RecordingText.Ended(Event(1, "finished", "max_size", 0), s));
            T.Check(RecordingText.Ended(Event(1, "finished", "disk_low", 0), s)!.Contains("almost full", StringComparison.Ordinal));
            T.Equal(null, RecordingText.Ended(Event(1, "finished", "core_shutdown", 0), s));
            T.Check(RecordingText.Ended(Event(1, "recovered", null, 0), s)!.Contains("unfinished", StringComparison.Ordinal));
            T.Equal("Recording stopped: no picture arrived", RecordingText.Ended(Event(1, "failed", "no_video", 0), s));
        });

        T.Run("recordings: the board follows starts and events", () =>
        {
            var cam = new CameraRef("d1", 2);
            var b = new RecordingBoard();
            b.Begin(7, "live", cam, "C:\\x\\a.mp4", "main");
            T.Equal("● REC 0:00", b.Pill(cam));
            T.Equal(7UL, b.LiveOf(cam)?.Id ?? 0);
            T.Check(b.CallOf(cam) is null);
            T.Check(b.Apply(Event(7, "recording", null, 83_000)) is null);
            T.Equal("● REC 1:23", b.Pill(cam));
            // A call of another camera; the events of an ID the app never started are ignored.
            var other = new CameraRef("d9", 2);
            b.Begin(8, "call", other, "C:\\x\\b.mp4", "sub");
            b.Apply(Event(8, "recording", null, 1000, "call"));
            b.Apply(Event(9, "recording", null, 1000));
            T.Equal(8UL, b.CallOf(other)?.Id ?? 0);
            T.Equal("7,8", string.Join(",", b.List.Select(r => r.Id)));
            b.Rekey("d1", "k1");
            T.Equal(7UL, b.LiveOf(new CameraRef("k1", 2))?.Id ?? 0);
            RecordingEvent? end = b.Apply(Event(7, "finished", "stopped", 90_000));
            T.Equal(7UL, end?.Recording ?? 0);
            T.Equal(7UL, b.Ended[^1].Recording);
            T.Equal(null, b.Pill(new CameraRef("k1", 2)));
            // An unknown recording's end is ignored; a recovered one is kept.
            T.Equal(null, b.Apply(Event(99, "finished", "stopped", 0)));
            T.Equal(99UL, b.Apply(Event(99, "recovered", null, 0))?.Recording ?? 0);
            // The pill shows the longest of a camera's recordings.
            b.Begin(20, "live", cam, "C:\\x\\c.mp4", "main");
            b.Begin(21, "call", cam, "C:\\x\\d.mp4", "main");
            b.Apply(Event(20, "recording", null, 5000));
            b.Apply(Event(21, "recording", null, 65_000, "call"));
            T.Equal("● REC 1:05", b.Pill(cam));
            // Only the newest ended recordings are kept.
            for (ulong i = 100; i < 100 + RecordingBoard.EndedKept + 5; i++)
            {
                b.Begin(i, "live", cam, "x", "main");
                b.Apply(Event(i, "finished", "stopped", 0));
            }
            T.Equal(RecordingBoard.EndedKept, b.Ended.Count);
        });

        T.Run("recordings: the header's pill stays and the menu follows the state", () =>
        {
            var tile = new TileHeaderTile { Audio = true, Talk = true, Doorbell = true, PanTilt = true, Presets = true, Lens = true, Fisheye = true, Recording = true, Zoomed = true };
            for (double width = 60; width <= 600; width += 10)
            {
                TileHeaderLayout layout = TileHeaderLayout.Compute(width, 200, tile, null, null);
                T.Equal(TileHeaderItem.RecordingPill, layout.Visible[0], $"width {width}");
                T.Check(!layout.Overflow.Contains(TileHeaderItem.RecordingPill));
            }
            IReadOnlyList<TileHeaderMenuEntry> menu = TileHeaderLayout.Menu(tile);
            T.Equal("Stop Recording", menu.First(e => e.Item == TileHeaderItem.Record).Label);
            T.Check(!menu.Any(e => e.Item is TileHeaderItem.RecordMain or TileHeaderItem.RecordSub));
            IReadOnlyList<TileHeaderMenuEntry> idle = TileHeaderLayout.Menu(tile with { Recording = false });
            T.Equal("Start Recording", idle.First(e => e.Item == TileHeaderItem.Record).Label);
            T.Check(idle.Any(e => e.Item == TileHeaderItem.RecordMain) && idle.Any(e => e.Item == TileHeaderItem.RecordSub));
            IReadOnlyList<TileHeaderMenuEntry> call = TileHeaderLayout.Menu(tile with { Call = TileHeaderCall.Window });
            T.Equal("Stop Recording Call", call.First(e => e.Item == TileHeaderItem.Record).Label);
        });

        T.Run("recordings: a call's recording and transcript share a stem, and old recordings are pruned", () =>
        {
            const string name = "2026-10-11 17.02.03 Camera 1.txt";
            T.Equal("2026-10-11 17.02.03 Camera 1", CallRecordingFiles.SharedStem(name, []));
            T.Equal("2026-10-11 17.02.03 Camera 1 2", CallRecordingFiles.SharedStem(name, ["2026-10-11 17.02.03 camera 1.MP4"]));
            T.Equal("2026-10-11 17.02.03 Camera 1 3", CallRecordingFiles.SharedStem(name,
                ["2026-10-11 17.02.03 Camera 1.txt", "2026-10-11 17.02.03 Camera 1 2.partial.mp4"]));

            DateTime t0 = new(2026, 10, 11, 0, 0, 0, DateTimeKind.Utc);
            var files = new List<StoredTranscript>
            {
                new("a.mp4", 100, t0), new("b.mp4", 100, t0.AddMinutes(1)), new("c.partial.mp4", 1_000_000_000_000, t0.AddMinutes(2)),
                new("d.txt", 100, t0.AddMinutes(3)),
            };
            T.Equal("", string.Join(",", CallRecordingFiles.ToDelete(files)));
            files.Add(new("e.mp4", 9_999_999_950, t0.AddMinutes(4)));
            // 10 GB: the newest (e) is kept, which leaves no room for the older two.
            T.Equal("a.mp4,b.mp4", string.Join(",", CallRecordingFiles.ToDelete(files)));
        });

        T.Run("recordings: a tone's pitch and level are measured", () =>
        {
            float[] sine = ToneCheck.Sine(1000, 0.25, 48_000, 48_000);
            short[] pcm = [.. sine.Select(f => (short)Math.Round(f * 32767))];
            double hz = ToneCheck.Frequency(pcm, 4800, 24_000, 48_000);
            T.Check(Math.Abs(hz - 1000) < 5, $"{hz} Hz");
            double db = ToneCheck.Decibels(ToneCheck.Rms(pcm, 4800, 24_000));
            // 0.25 amplitude: 0.177 RMS, -15 dB.
            T.Check(Math.Abs(db - -15.05) < 0.1, $"{db} dB");
            T.Equal(0.0, ToneCheck.Frequency(new short[100], 0, 100, 48_000));
            T.Equal(-120.0, ToneCheck.Decibels(0));
        });

        T.Run("recordings: the record-from names", () =>
        {
            T.Equal("main", RecordQuality.Main.Name());
            T.Equal("Sub-stream", RecordQuality.Sub.Title());
            T.Equal(RecordQuality.Sub, RecordQualityNames.Parse("sub"));
            T.Equal(null, RecordQualityNames.Parse("mobile"));
        });
    }
}
