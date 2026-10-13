using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// The recording player's stream choice (docs/UX.md "Recordings", "Quality"): Auto's rule by layout, speed and zoom, the
/// forced High and Low, which cameras change, and what leaves with a camera. The macOS app has the same tests
/// (PlayerQualityTests.swift).
/// </summary>
internal static class PlayerStreamsTests
{
    private static readonly RecordingCamera A = new("dev", 0);
    private static readonly RecordingCamera B = new("dev", 1);
    private static readonly RecordingCamera C = new("dev", 2);

    private const NvStreamTier Main = NvStreamTier.Main;
    private const NvStreamTier Sub = NvStreamTier.Sub;

    private static List<NvStreamTier> Tiers(PlayerStreams s, RecordingCamera[] cams, PlayerGridMode mode, double rate = 1, Dictionary<RecordingCamera, double>? zoom = null) =>
        s.Opening(cams, mode, rate, c => zoom is not null && zoom.TryGetValue(c, out double z) ? z : 1);

    private static void Same<TItem>(IEnumerable<TItem> expected, IEnumerable<TItem> actual, string? note = null) =>
        T.Equal(string.Join(", ", expected), string.Join(", ", actual), note);

    public static void Run()
    {
        T.Run("player streams: Auto is the sub-stream in the grid and the main stream alone", () =>
        {
            var s = new PlayerStreams();
            Same(new[] { Sub, Sub, Sub }, Tiers(s, [A, B, C], PlayerGridMode.Tiled));
            Same(new[] { Sub, Sub, Sub }, Tiers(s, [A, B, C], new PlayerGridMode.Emphasis(1)));
            Same(new[] { Sub, Main, Sub }, Tiers(s, [A, B, C], new PlayerGridMode.Single(1)));
            Same(new[] { Main }, Tiers(s, [A], PlayerGridMode.Tiled), "one camera is a large view");
        });

        T.Run("player streams: Auto uses the sub-stream from twice the speed", () =>
        {
            var s = new PlayerStreams();
            var single = new PlayerGridMode.Single(0);
            Same(new[] { Main, Sub }, Tiers(s, [A, B], single, 0.25));
            Same(new[] { Main, Sub }, Tiers(s, [A, B], single, 1));
            Same(new[] { Sub, Sub }, Tiers(s, [A, B], single, 2));
            Same(new[] { Sub, Sub }, Tiers(s, [A, B], single, 16));
        });

        T.Run("player streams: Auto uses the main stream when zoomed past 2x, with hysteresis", () =>
        {
            var s = new PlayerStreams();
            RecordingCamera[] cams = [A, B];
            NvStreamTier Wanted(double scale, double rate = 1) => s.Wanted(A, cams, PlayerGridMode.Tiled, rate, scale);
            T.Equal(Sub, Wanted(1.9));
            T.Equal(Main, Wanted(2.0));
            T.Equal(Main, Wanted(1.7), "stays main until below 1.6x");
            T.Equal(Sub, Wanted(1.5));
            T.Equal(Sub, Wanted(2.5, rate: 4), "speed wins");
            T.Equal(Sub, Wanted(1.0));
        });

        T.Run("player streams: High and Low are forced whatever the layout, speed or zoom", () =>
        {
            var s = new PlayerStreams();
            s.SetQuality(A, PlayerQuality.High);
            s.SetQuality(B, PlayerQuality.Low);
            Same(new[] { Main, Sub }, Tiers(s, [A, B], PlayerGridMode.Tiled, 16));
            Same(new[] { Main, Sub }, Tiers(s, [A, B], new PlayerGridMode.Single(1), 1, new() { [B] = 4 }));
            s.SetQuality(A, PlayerQuality.Auto);
            T.Equal(PlayerQuality.Auto, s.Quality(A));
            T.Equal(Sub, s.Wanted(A, [A, B], PlayerGridMode.Tiled, 1, 1));
        });

        T.Run("player streams: only the cameras whose stream changed are reported", () =>
        {
            var s = new PlayerStreams();
            RecordingCamera[] cams = [A, B, C];
            Tiers(s, cams, PlayerGridMode.Tiled);
            List<string> Changes(PlayerGridMode mode, double rate = 1) =>
                [.. s.Changes(cams, mode, rate, _ => 1).Select(c => $"{c.Camera.Channel} {c.Tier}")];
            T.Equal(0, Changes(PlayerGridMode.Tiled).Count);
            Same(new[] { "2 Main" }, Changes(new PlayerGridMode.Single(2)));
            T.Equal(0, Changes(new PlayerGridMode.Single(2)).Count, "applied once");
            s.SetQuality(A, PlayerQuality.High);
            Same(new[] { "0 Main" }, Changes(new PlayerGridMode.Single(2)));
            Same(new[] { "2 Sub" }, Changes(new PlayerGridMode.Single(2), 4));
            T.Equal(0, Changes(PlayerGridMode.Tiled, 1).Count);
            s.SetQuality(A, PlayerQuality.Auto);
            Same(new[] { "0 Sub" }, Changes(PlayerGridMode.Tiled));
            T.Equal(Sub, s.Applied(A));
            T.Equal(Sub, s.Applied(C));
        });

        T.Run("player streams: a camera that leaves forgets its choice", () =>
        {
            var s = new PlayerStreams();
            s.SetQuality(A, PlayerQuality.High);
            Tiers(s, [A, B], PlayerGridMode.Tiled);
            s.Retain([B]);
            T.Equal(PlayerQuality.Auto, s.Quality(A));
            T.Check(s.Applied(A) is null);
            T.Equal(Sub, s.Applied(B));
        });

        T.Run("player streams: the header names the stream", () =>
        {
            var s = new PlayerStreams();
            Tiers(s, [A, B], new PlayerGridMode.Single(0));
            T.Equal("main stream", s.Label(A));
            T.Equal("sub stream", s.Label(B));
            s.SetQuality(A, PlayerQuality.Low);
            s.Changes([A, B], new PlayerGridMode.Single(0), 1, _ => 1);
            T.Equal("Low · sub stream", s.Label(A));
        });
    }
}
