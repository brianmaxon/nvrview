// --bench: the decoder alone (no core, no renderer). Decodes a file's
// access units in a loop as fast as the decoder takes them, each into the
// tile picture as the app would, and reports time per frame (CPU side),
// throughput including a final wait for the GPU, and CPU time per frame.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Video;

namespace NvrView.Demo;

internal static class Bench
{
    private sealed class Answers : IDecodeResults
    {
        public long Decoded;
        public long Failed;
        public long NoPicture;

        void IDecodeResults.Decoded(long ptsUs) => Decoded++;

        void IDecodeResults.Failed(NvDecodeFailure kind)
        {
            if (kind == NvDecodeFailure.NoPicture)
            {
                NoPicture++;
            }
            else
            {
                Failed++;
            }
        }
    }

    public static int Run(Options o)
    {
        string file = o.Files[0];
        NvCodec codec = Path.GetExtension(file) is ".h265" or ".hevc" ? NvCodec.H265 : NvCodec.H264;
        byte[] data = File.ReadAllBytes(file);
        List<AccessUnit> units = AccessUnits.Split(data, codec);
        if (units.Count == 0 || !units[0].Keyframe)
        {
            Console.Error.WriteLine("bench: the file must start with a keyframe");
            return 1;
        }
        using GpuDevice gpu = GpuDevice.Create(o.Gpu);
        var picture = new TilePicture(gpu);
        using var decoder = new MfDecoder(gpu, picture, !o.Software);
        try
        {
            if (decoder.Configure(codec, [], o.Coded) is { } failure)
            {
                Console.Error.WriteLine($"bench: configure failed ({failure}): {decoder.ConfigureError}");
                return 1;
            }
            var answers = new Answers();
            int frames = o.Frames;
            long[] ticks = new long[frames];
            using Process self = Process.GetCurrentProcess();
            TimeSpan cpu0 = self.TotalProcessorTime;
            long start = Stopwatch.GetTimestamp();
            for (int i = 0; i < frames; i++)
            {
                AccessUnit u = units[i % units.Count];
                long t0 = Stopwatch.GetTimestamp();
                decoder.Decode(data.AsSpan(u.Offset, u.Length), u.Keyframe, i * 50_000L, answers);
                ticks[i] = Stopwatch.GetTimestamp() - t0;
            }
            gpu.WaitForIdle();
            double wall = Stopwatch.GetElapsedTime(start).TotalSeconds;
            self.Refresh();
            double cpu = (self.TotalProcessorTime - cpu0).TotalSeconds;

            // The first frames include decoder start-up; leave them out of the distribution.
            long[] steady = ticks.Skip(Math.Min(10, frames / 10)).Order().ToArray();
            double Ms(long t) => t * 1000.0 / Stopwatch.Frequency;
            double Pct(double p) => Ms(steady[Math.Min(steady.Length - 1, (int)(p * steady.Length))]);
            NvSize size = decoder.OutputSize;
            Console.WriteLine($"device: {gpu.Describe()}");
            Console.WriteLine(
                $"bench {Path.GetFileName(file)}: {size.Width}x{size.Height} {codec}, {frames} frames, " +
                $"decoder {Program.KindName(decoder.Kind)} ({decoder.Name})");
            Console.WriteLine(
                $"  decoded {answers.Decoded}, failed {answers.Failed}, no picture {answers.NoPicture}, " +
                $"held too long {decoder.Counters.HeldTooLong}, input allocations {decoder.Counters.InputAllocations}" +
                (decoder.LastError != 0 ? $", last error 0x{decoder.LastError:X8}" : ""));
            Console.WriteLine(
                $"  per frame: mean {steady.Average(t => Ms(t)):0.00} ms, median {Pct(0.5):0.00}, p95 {Pct(0.95):0.00}, max {Ms(steady[^1]):0.00}");
            Console.WriteLine(
                $"  throughput {frames / wall:0} fps, cpu {cpu * 1000 / frames:0.00} ms per frame ({cpu / wall * 100:0}% of one core)");
            bool ok = answers.Failed == 0 && answers.Decoded > frames * 9 / 10 && Program.Matches(o.Expect, decoder.Kind);
            Console.WriteLine(ok ? "bench: PASS" : "bench: FAIL");
            return ok ? 0 : 1;
        }
        finally
        {
            decoder.Dispose();
            using (gpu.Lock())
            {
                picture.Dispose();
            }
        }
    }
}
