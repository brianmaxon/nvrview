// --selftest: plays each file as its own mock-nvr channel through the core,
// Media Foundation, and the grid renderer, and checks real pixels read back
// from the render target, like the macOS demo's Metal self-test:
//
// - every tile decodes, keeps decoding, and has no decode failures;
// - its picture fills its rectangle with the picture's aspect ratio, with
//   black just outside;
// - the picture is not flat and differs from at least one of two earlier
//   readbacks taken 0.12 s apart;
// - the known-color fixtures (*colors*) come out in the colors they were
//   made from (the matrix, the range, and the NV12 planes together);
// - the tier switch at a quarter of the time completed (the tile shows
//   generation 1, the old generation is gone);
// - with --expect, every tile decoded on the hardware (or software) path.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Video;

namespace NvrView.Demo;

internal static class SelfTest
{
    /// <summary>COLOR_BARS in fixtures/synthetic/make-video.sh, left to right; 32 x 64 px each.</summary>
    private static readonly (int R, int G, int B)[] Bars =
    [
        (0xB4, 0x28, 0x28), (0x28, 0xA0, 0x3C), (0x32, 0x3C, 0xBE),
        (0xDC, 0xDC, 0xDC), (0x80, 0x80, 0x80), (0x18, 0x18, 0x18),
    ];

    // The macOS test allows 4 at the picture's own size; the grid scales it.
    private const double BarTolerance = 8;

    public static int Run(Options o)
    {
        var mocks = new List<MockNvr>();
        GpuDevice? gpu = null;
        Session? session = null;
        try
        {
            foreach (string file in o.Files)
            {
                mocks.Add(MockNvr.Start(o.MockNvr!, Program.Password, "--channels", "1", "--fixture", file, "--fps", "20"));
            }
            gpu = GpuDevice.Create(o.Gpu);
            Console.WriteLine($"device: {gpu.Describe()}");
            session = new Session(gpu, o.Width, o.Height, !o.Software, Program.Password);
            for (int i = 0; i < mocks.Count; i++)
            {
                ulong device = session.AddDevice(mocks[i].Host, mocks[i].Port);
                session.OpenTile(Path.GetFileName(o.Files[i]), device, 0, NvStreamTier.Sub);
            }
            session.StartRendering();
            var clock = Stopwatch.StartNew();
            void Until(double seconds)
            {
                double wait = seconds - clock.Elapsed.TotalSeconds;
                if (wait > 0)
                {
                    Thread.Sleep(TimeSpan.FromSeconds(wait));
                }
            }

            Until(o.Seconds * 0.25);
            foreach (DemoTile t in session.Tiles)
            {
                session.Core.SetTier(t.Id, NvStreamTier.Main);
            }
            Until(o.Seconds * 0.5);
            Capture a = session.CaptureNext();
            Until(o.Seconds * 0.5 + 0.12);
            Capture b = session.CaptureNext();
            Until(o.Seconds);
            Capture c = session.CaptureNext();
            return Evaluate(o, session, [a, b], c);
        }
        finally
        {
            session?.Dispose();
            gpu?.Dispose();
            foreach (MockNvr m in mocks)
            {
                m.Dispose();
            }
        }
    }

    private static int Evaluate(Options o, Session session, Capture[] earlier, Capture final)
    {
        var failures = new List<string>();
        RenderedImage image = final.Image;
        Console.WriteLine(
            $"render target: {image.Width}x{image.Height}, frames {session.RenderFrames}, " +
            $"{session.RenderMs:0.00} ms per frame");
        for (int i = 0; i < session.Tiles.Count; i++)
        {
            DemoTile tile = session.Tiles[i];
            TileCounters c = tile.Video.Counters;
            CoreTileStats stats = session.StatsOf(tile.Id);
            var problems = new List<string>();
            NvRect rect = final.Layout[i].Dest;
            NvSize size = final.Sizes[i];
            if (c.Decoded == 0)
            {
                problems.Add("nothing decoded");
            }
            if (final.Decoded[i] <= earlier[0].Decoded[i])
            {
                problems.Add("decoding stopped");
            }
            if (c.Failures > 0 || c.ConfigureFailures > 0)
            {
                problems.Add($"decode failures: {c.Failures}, configure failures: {c.ConfigureFailures}" +
                    (tile.Video.ConfigureError is { } why ? $" ({why})" : ""));
            }
            if (size.Width == 0 || rect.Width == 0 || rect.Height == 0)
            {
                problems.Add("no picture");
            }
            else
            {
                double aspect = rect.Width / (double)rect.Height / (size.Width / (double)size.Height);
                if (Math.Abs(aspect - 1) > 0.02)
                {
                    problems.Add($"rectangle {Show(rect)} does not match a {size.Width}x{size.Height} picture");
                }
                problems.AddRange(EdgeProblems(image, rect, final.Layout));
            }
            double spread = Spread(image, rect);
            if (spread < 8)
            {
                problems.Add($"picture is flat (spread {spread:0.0})");
            }
            byte[] now = Region(image, rect);
            bool unchanged = earlier.All(e => e.Layout[i].Dest.Equals(rect) && Region(e.Image, rect).AsSpan().SequenceEqual(now));
            if (unchanged)
            {
                problems.Add("picture did not change");
            }
            if (tile.Video.ShownGeneration != 1 || c.Generations < 2)
            {
                problems.Add($"the tier switch did not complete (showing generation {tile.Video.ShownGeneration}, {c.Generations} generations)");
            }
            DecoderKind kind = tile.Video.DecoderKind;
            if (!Program.Matches(o.Expect, kind))
            {
                problems.Add($"decoded on the {Program.KindName(kind)} path, expected {o.Expect}");
            }
            string colors = "";
            if (tile.Name.Contains("colors", StringComparison.Ordinal) && size.Width > 0)
            {
                double worst = BarError(image, final.Layout[i]);
                colors = $", bar error {worst:0.0}";
                if (worst > BarTolerance)
                {
                    problems.Add($"colors are off by up to {worst:0.0} (allowed {BarTolerance})");
                }
            }
            double decodeMs = c.Decodes == 0 ? 0 : c.DecodeTicks * 1000.0 / Stopwatch.Frequency / c.Decodes;
            Console.WriteLine(
                $"tile {i} {tile.Name}: in {stats.Received}, decoded {c.Decoded}, shown {tile.Shown}, " +
                $"gated {stats.Gated}, dropped {stats.Dropped}, format changes {Math.Max(0, c.Configures - c.Generations)}, " +
                $"failures {c.Failures}, no picture {c.NoPictures}, " +
                $"decoder {Program.KindName(kind)} ({tile.Video.DecoderName}), " +
                $"picture {(size.Width > 0 ? $"{size.Width}x{size.Height}" : "none")} in {Show(rect)}, " +
                $"decode {decodeMs:0.00} ms, spread {spread:0.0}{colors} " +
                (problems.Count == 0 ? "OK" : "FAIL"));
            failures.AddRange(problems.Select(p => $"tile {i}: {p}"));
        }
        failures.AddRange(session.Errors.Select(e => $"core: {e}"));
        if (session.Core.CallbackExceptionCount > 0)
        {
            failures.Add($"callbacks threw {session.Core.CallbackExceptionCount} times: {session.Core.FirstCallbackException}");
        }
        if (failures.Count == 0)
        {
            Console.WriteLine("selftest: PASS");
            return 0;
        }
        foreach (string f in failures)
        {
            Console.WriteLine($"  {f}");
        }
        Console.WriteLine("selftest: FAIL");
        return 1;
    }

    private static string Show(NvRect r) => $"{r.X},{r.Y} {r.Width}x{r.Height}";

    // The picture reaches every edge of its rectangle (some lit pixels on
    // each edge) and nothing is drawn just outside it.
    private static List<string> EdgeProblems(RenderedImage image, NvRect r, NvTileLayout[] all)
    {
        int x0 = (int)r.X, y0 = (int)r.Y, x1 = (int)(r.X + r.Width) - 1, y1 = (int)(r.Y + r.Height) - 1;
        var problems = new List<string>();
        if (x0 < 0 || y0 < 0 || x1 >= image.Width || y1 >= image.Height || x1 <= x0 || y1 <= y0)
        {
            problems.Add($"rectangle {Show(r)} is outside the {image.Width}x{image.Height} frame");
            return problems;
        }
        bool Lit(int x, int y)
        {
            (int cr, int cg, int cb) = image.Rgb(x, y);
            return cr + cg + cb > 24;
        }
        (string Name, int Lit, int Total)[] edges =
        [
            ("top", Enumerable.Range(x0, x1 - x0 + 1).Count(x => Lit(x, y0)), x1 - x0 + 1),
            ("bottom", Enumerable.Range(x0, x1 - x0 + 1).Count(x => Lit(x, y1)), x1 - x0 + 1),
            ("left", Enumerable.Range(y0, y1 - y0 + 1).Count(y => Lit(x0, y)), y1 - y0 + 1),
            ("right", Enumerable.Range(y0, y1 - y0 + 1).Count(y => Lit(x1, y)), y1 - y0 + 1),
        ];
        foreach ((string name, int lit, int total) in edges)
        {
            if (lit * 4 < total)
            {
                problems.Add($"picture does not reach the {name} edge");
            }
        }
        (int X, int Y)[] outside = [(x0 - 1, (y0 + y1) / 2), (x1 + 1, (y0 + y1) / 2), ((x0 + x1) / 2, y0 - 1), ((x0 + x1) / 2, y1 + 1)];
        foreach ((int x, int y) in outside)
        {
            if (x < 0 || y < 0 || x >= image.Width || y >= image.Height)
            {
                continue;
            }
            bool inOther = all.Any(t => x >= t.Dest.X && x < t.Dest.X + t.Dest.Width && y >= t.Dest.Y && y < t.Dest.Y + t.Dest.Height);
            if (!inOther && image.Rgb(x, y) != (0, 0, 0))
            {
                problems.Add($"drawn outside its rectangle at {x},{y}");
            }
        }
        return problems;
    }

    private static byte[] Region(RenderedImage image, NvRect r)
    {
        int w = (int)Math.Min(r.Width, (uint)Math.Max(0, image.Width - (int)r.X));
        int h = (int)Math.Min(r.Height, (uint)Math.Max(0, image.Height - (int)r.Y));
        byte[] out_ = new byte[Math.Max(0, w * h * 4)];
        for (int y = 0; y < h; y++)
        {
            Array.Copy(image.Bgra, (((int)r.Y + y) * image.Width + (int)r.X) * 4, out_, y * w * 4, w * 4);
        }
        return out_;
    }

    // Standard deviation of brightness over a rectangle, sampled every 2 px.
    private static double Spread(RenderedImage image, NvRect r)
    {
        double sum = 0, sumSq = 0, n = 0;
        for (int y = (int)r.Y; y < Math.Min(r.Y + r.Height, (uint)image.Height); y += 2)
        {
            for (int x = (int)r.X; x < Math.Min(r.X + r.Width, (uint)image.Width); x += 2)
            {
                (int cr, int cg, int cb) = image.Rgb(x, y);
                double v = (cr + cg + cb) / 3.0;
                sum += v;
                sumSq += v * v;
                n++;
            }
        }
        if (n == 0)
        {
            return 0;
        }
        double mean = sum / n;
        return Math.Sqrt(Math.Max(sumSq / n - mean * mean, 0));
    }

    // Largest per-channel error over the middle of each bar, mapped from
    // picture pixels to the render target through the tile's layout.
    private static double BarError(RenderedImage image, NvTileLayout l)
    {
        double sx = l.Dest.Width / (double)l.Source.Width, sy = l.Dest.Height / (double)l.Source.Height;
        double worst = 0;
        for (int i = 0; i < Bars.Length; i++)
        {
            // An 8x8 patch in the middle of the bar, clear of chroma edges.
            int px0 = 32 * i + 12, py0 = 28;
            int x0 = (int)Math.Ceiling(l.Dest.X + (px0 - l.Source.X) * sx);
            int x1 = (int)Math.Floor(l.Dest.X + (px0 + 8 - l.Source.X) * sx);
            int y0 = (int)Math.Ceiling(l.Dest.Y + (py0 - l.Source.Y) * sy);
            int y1 = (int)Math.Floor(l.Dest.Y + (py0 + 8 - l.Source.Y) * sy);
            double r = 0, g = 0, b = 0;
            int n = 0;
            for (int y = y0; y < y1; y++)
            {
                for (int x = x0; x < x1; x++)
                {
                    (int cr, int cg, int cb) = image.Rgb(x, y);
                    r += cr;
                    g += cg;
                    b += cb;
                    n++;
                }
            }
            if (n == 0)
            {
                return double.PositiveInfinity;
            }
            (int er, int eg, int eb) = Bars[i];
            worst = Math.Max(worst, Math.Max(Math.Abs(r / n - er), Math.Max(Math.Abs(g / n - eg), Math.Abs(b / n - eb))));
        }
        return worst;
    }
}
