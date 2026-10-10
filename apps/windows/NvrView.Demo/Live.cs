// --live: N tiles of one mock-nvr (every channel serving the same fixture)
// through the core, Media Foundation, and the grid renderer, offscreen.
// After a warm-up it measures, over the rest of the run: decoded and drawn
// frames per second per tile, time per Decode action (submit, decode, copy
// into the tile's picture, answer), process CPU, and memory.

using System.Diagnostics;
using System.Globalization;
using NvrView.Video;

namespace NvrView.Demo;

internal static class Live
{
    private sealed record Snapshot(double Wall, TimeSpan Cpu, TimeSpan MockCpu, long RenderFrames, long Allocated, TileCounters[] Tiles, long[] Shown);

    public static int Run(Options o)
    {
        string fps = o.Fps.ToString(CultureInfo.InvariantCulture);
        using MockNvr mock = MockNvr.Start(o.MockNvr!, Program.Password,
            "--channels", o.Channels.ToString(CultureInfo.InvariantCulture), "--fixture", o.Fixture!, "--fps", fps);
        using GpuDevice gpu = GpuDevice.Create(o.Gpu);
        Console.WriteLine($"device: {gpu.Describe()}");
        Console.WriteLine($"stream: {Path.GetFileName(o.Fixture)} at {fps} fps, {o.Channels} tiles, {o.Tier} tier, render {o.Width}x{o.Height} at 30 fps");
        using var session = new Session(gpu, o.Width, o.Height, !o.Software, Program.Password);
        ulong device = session.AddDevice(mock.Host, mock.Port);
        for (int ch = 0; ch < o.Channels; ch++)
        {
            session.OpenTile($"channel {ch}", device, (byte)ch, o.Tier);
        }
        session.StartRendering();
        var clock = Stopwatch.StartNew();
        using Process self = Process.GetCurrentProcess();
        Snapshot Take()
        {
            self.Refresh();
            mock.Process.Refresh();
            return new Snapshot(
                clock.Elapsed.TotalSeconds, self.TotalProcessorTime, mock.Process.TotalProcessorTime, session.RenderFrames,
                GC.GetTotalAllocatedBytes(precise: true),
                [.. session.Tiles.Select(t => t.Video.Counters)], [.. session.Tiles.Select(t => Interlocked.Read(ref t.Shown))]);
        }

        double warmup = Math.Min(3, o.Seconds / 3);
        Thread.Sleep(TimeSpan.FromSeconds(warmup));
        Snapshot a = Take();
        Thread.Sleep(TimeSpan.FromSeconds(o.Seconds - warmup));
        Snapshot b = Take();
        self.Refresh();
        long workingSet = self.WorkingSet64, peakWorkingSet = self.PeakWorkingSet64, privateBytes = self.PrivateMemorySize64;

        double dt = b.Wall - a.Wall;
        var problems = new List<string>();
        var rates = new List<double>();
        var decodeMs = new List<double>();
        for (int i = 0; i < session.Tiles.Count; i++)
        {
            DemoTile tile = session.Tiles[i];
            TileCounters ta = a.Tiles[i], tb = b.Tiles[i];
            double rate = (tb.Decoded - ta.Decoded) / dt;
            double shown = (b.Shown[i] - a.Shown[i]) / dt;
            long decodes = tb.Decodes - ta.Decodes;
            double ms = decodes == 0 ? 0 : (tb.DecodeTicks - ta.DecodeTicks) * 1000.0 / Stopwatch.Frequency / decodes;
            double maxMs = tb.MaxDecodeTicks * 1000.0 / Stopwatch.Frequency;
            rates.Add(rate);
            decodeMs.Add(ms);
            DecoderKind kind = tile.Video.DecoderKind;
            Console.WriteLine(
                $"tile {i}: decoded {rate:0.0}/s, drawn {shown:0.0}/s, decode {ms:0.00} ms (max {maxMs:0.0} including start-up), " +
                $"failures {tb.Failures}, no picture {tb.NoPictures}, input allocations {tb.InputAllocations}, " +
                $"{Program.KindName(kind)}");
            if (rate < 1)
            {
                problems.Add($"tile {i}: not decoding");
            }
            if (tb.Failures > 0 || tb.ConfigureFailures > 0)
            {
                problems.Add($"tile {i}: {tb.Failures} decode failures, {tb.ConfigureFailures} configure failures");
            }
            if (!Program.Matches(o.Expect, kind))
            {
                problems.Add($"tile {i}: decoded on the {Program.KindName(kind)} path, expected {o.Expect}");
            }
        }
        double cpu = (b.Cpu - a.Cpu).TotalSeconds / dt;
        double mockCpu = (b.MockCpu - a.MockCpu).TotalSeconds / dt;
        double renderFps = (b.RenderFrames - a.RenderFrames) / dt;
        int cores = Environment.ProcessorCount;
        Console.WriteLine(
            $"summary: {session.Tiles.Count} tiles over {dt:0.0} s: decoded {rates.Average():0.0}/s per tile (min {rates.Min():0.0}), " +
            $"decode {decodeMs.Average():0.00} ms per frame, render {renderFps:0.0} fps at {session.RenderMs:0.00} ms");
        Console.WriteLine(
            $"cpu: viewer {cpu * 100 / cores:0.0}% of {cores} logical processors ({cpu:0.00} cores), " +
            $"mock-nvr {mockCpu * 100 / cores:0.0}%");
        long frames = b.Tiles.Zip(a.Tiles, (x, y) => x.Decoded - y.Decoded).Sum();
        Console.WriteLine(
            $"memory: working set {workingSet >> 20} MB (peak {peakWorkingSet >> 20} MB), private {privateBytes >> 20} MB, " +
            $"managed allocations {(b.Allocated - a.Allocated) / dt / 1024:0.0} KB/s " +
            $"({(frames == 0 ? 0 : (b.Allocated - a.Allocated) / (double)frames):0} bytes per decoded frame, event parsing included)");
        problems.AddRange(session.Errors.Select(e => $"core: {e}"));
        if (problems.Count == 0)
        {
            Console.WriteLine("live: PASS");
            return 0;
        }
        foreach (string p in problems)
        {
            Console.WriteLine($"  {p}");
        }
        Console.WriteLine("live: FAIL");
        return 1;
    }
}
