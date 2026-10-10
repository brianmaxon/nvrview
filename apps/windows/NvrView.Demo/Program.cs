// Headless demo of the Windows video layer (NvrView.Video): no window, the
// grid is drawn offscreen and checked by reading pixels back.
//
//   NvrView.Demo --mock-nvr EXE [common options] --selftest SECONDS FILE...
//       One mock-nvr per file (each serving it as channel 0), one tile each,
//       played through the core and Media Foundation for SECONDS, with a
//       tier switch at a quarter of the time. Prints one line per tile and
//       PASS or FAIL (exit code 0 or 1), like the macOS demo's --selftest.
//   NvrView.Demo --mock-nvr EXE [common options] --live SECONDS
//                [--channels N] [--fixture FILE] [--fps N] [--tier sub|main]
//       One mock-nvr with N channels, N tiles; reports frame rates, decode
//       time, CPU, and memory.
//   NvrView.Demo [common options] --bench FILE [--frames N] [--coded WxH]
//       Decoder only (no core): decodes FILE in a loop as fast as possible.
//       --coded gives the coded size, which the core supplies otherwise (the
//       H.265 decoder needs it).
//
// Common options: --lib PATH (nvrview.dll), --gpu auto|hardware|warp,
// --software (decode on the CPU even with a GPU), --size WxH (render target),
// --expect hardware|software (fail unless every tile decoded that way).
// The password comes from NVRVIEW_TEST_PASSWORD (default example-password).

using System.Globalization;
using System.Runtime.InteropServices;
using NvrView.Interop;
using NvrView.Video;

namespace NvrView.Demo;

internal enum Mode
{
    SelfTest,
    Live,
    Bench,
}

internal sealed class Options
{
    public const string Usage =
        "usage: NvrView.Demo --mock-nvr EXE [options] --selftest SECONDS FILE...\n" +
        "       NvrView.Demo --mock-nvr EXE [options] --live SECONDS [--channels N] [--fixture FILE] [--fps N] [--tier sub|main]\n" +
        "       NvrView.Demo [options] --bench FILE [--frames N] [--coded WxH]\n" +
        "options: --lib PATH  --gpu auto|hardware|warp  --software  --size WxH  --expect hardware|software";

    public Mode Mode { get; private set; }

    public double Seconds { get; private set; }

    public List<string> Files { get; } = [];

    public string? MockNvr { get; private set; }

    public string? Lib { get; private set; }

    public GpuChoice Gpu { get; private set; } = GpuChoice.Auto;

    public bool Software { get; private set; }

    public uint Width { get; private set; } = 960;

    public uint Height { get; private set; } = 720;

    public string? Expect { get; private set; }

    public int Channels { get; private set; } = 4;

    public string? Fixture { get; private set; }

    public int Fps { get; private set; } = 15;

    public NvStreamTier Tier { get; private set; } = NvStreamTier.Sub;

    public int Frames { get; private set; } = 600;

    public NvSize Coded { get; private set; }

    public static Options Parse(string[] args)
    {
        var o = new Options();
        bool modeSet = false;
        for (int i = 0; i < args.Length; i++)
        {
            string a = args[i];
            string Value() => i + 1 < args.Length ? args[++i] : throw new ArgumentException($"{a} needs a value");
            double Positive() => double.TryParse(Value(), NumberStyles.Float, CultureInfo.InvariantCulture, out double v) && v > 0
                ? v : throw new ArgumentException($"bad value for {a}");
            int Count() => int.TryParse(Value(), NumberStyles.None, CultureInfo.InvariantCulture, out int v) && v > 0
                ? v : throw new ArgumentException($"bad value for {a}");
            switch (a)
            {
                case "--selftest":
                    o.Mode = Mode.SelfTest;
                    o.Seconds = Positive();
                    modeSet = true;
                    break;
                case "--live":
                    o.Mode = Mode.Live;
                    o.Seconds = Positive();
                    modeSet = true;
                    break;
                case "--bench":
                    o.Mode = Mode.Bench;
                    o.Files.Add(Value());
                    modeSet = true;
                    break;
                case "--mock-nvr": o.MockNvr = Path.GetFullPath(Value()); break;
                case "--lib": o.Lib = Path.GetFullPath(Value()); break;
                case "--gpu":
                    o.Gpu = Value() switch
                    {
                        "auto" => GpuChoice.Auto,
                        "hardware" => GpuChoice.Hardware,
                        "warp" => GpuChoice.Warp,
                        _ => throw new ArgumentException("--gpu is auto, hardware, or warp"),
                    };
                    break;
                case "--software": o.Software = true; break;
                case "--size":
                    string[] wh = Value().Split('x');
                    if (wh.Length != 2 || !uint.TryParse(wh[0], out uint w) || !uint.TryParse(wh[1], out uint h) ||
                        w < 64 || h < 64 || w > 8192 || h > 8192)
                    {
                        throw new ArgumentException("--size is WxH, 64 to 8192 each");
                    }
                    (o.Width, o.Height) = (w, h);
                    break;
                case "--expect":
                    o.Expect = Value();
                    if (o.Expect is not ("hardware" or "software"))
                    {
                        throw new ArgumentException("--expect is hardware or software");
                    }
                    break;
                case "--channels": o.Channels = Count(); break;
                case "--fixture": o.Fixture = Path.GetFullPath(Value()); break;
                case "--fps": o.Fps = Count(); break;
                case "--frames": o.Frames = Count(); break;
                case "--coded":
                    string[] cwh = Value().Split('x');
                    if (cwh.Length != 2 || !uint.TryParse(cwh[0], out uint cw) || !uint.TryParse(cwh[1], out uint ch))
                    {
                        throw new ArgumentException("--coded is WxH");
                    }
                    o.Coded = new NvSize(cw, ch);
                    break;
                case "--tier":
                    o.Tier = Value() switch
                    {
                        "sub" => NvStreamTier.Sub,
                        "main" => NvStreamTier.Main,
                        _ => throw new ArgumentException("--tier is sub or main"),
                    };
                    break;
                default:
                    if (a.StartsWith("--", StringComparison.Ordinal))
                    {
                        throw new ArgumentException($"unknown option {a}");
                    }
                    o.Files.Add(Path.GetFullPath(a));
                    break;
            }
        }
        if (!modeSet)
        {
            throw new ArgumentException("give --selftest, --live, or --bench");
        }
        if (o.Mode != Mode.Bench && o.MockNvr is null)
        {
            throw new ArgumentException("--selftest and --live need --mock-nvr EXE");
        }
        if (o.Mode == Mode.SelfTest && o.Files.Count == 0)
        {
            throw new ArgumentException("--selftest needs at least one .h264 or .h265 file");
        }
        if (o.Mode == Mode.Live && o.Fixture is null)
        {
            throw new ArgumentException("--live needs --fixture FILE");
        }
        if (o.Channels > 64)
        {
            throw new ArgumentException("--channels is at most 64");
        }
        return o;
    }
}

internal static class Program
{
    private static int Main(string[] args)
    {
        Options options;
        try
        {
            options = Options.Parse(args);
        }
        catch (ArgumentException e)
        {
            Console.Error.WriteLine($"demo: {e.Message}");
            Console.Error.WriteLine(Options.Usage);
            return 2;
        }
        if (options.Lib is { } lib)
        {
            NativeLibrary.SetDllImportResolver(typeof(NvNative).Assembly, (name, _, _) =>
                name == NvNative.Library ? NativeLibrary.Load(lib) : 0);
        }
        // A stuck run must end (CI).
        double limit = (options.Mode == Mode.Bench ? 300 : options.Seconds) + 60;
        using var watchdog = new Timer(_ =>
        {
            Console.Error.WriteLine($"demo: timed out after {limit:0} s");
            Environment.Exit(1);
        }, null, TimeSpan.FromSeconds(limit), Timeout.InfiniteTimeSpan);
        try
        {
            return options.Mode switch
            {
                Mode.SelfTest => SelfTest.Run(options),
                Mode.Live => Live.Run(options),
                _ => Bench.Run(options),
            };
        }
        catch (Exception e) when (e is VideoException or NvException or InvalidOperationException or IOException)
        {
            Console.Error.WriteLine($"demo: {e.Message}");
            Console.WriteLine("selftest: FAIL");
            return 1;
        }
    }

    public static string Password =>
        Environment.GetEnvironmentVariable("NVRVIEW_TEST_PASSWORD") is { Length: > 0 } p ? p : "example-password";

    public static string KindName(DecoderKind kind) => kind switch
    {
        DecoderKind.Hardware => "hardware",
        DecoderKind.SoftwareTexture => "software (texture output)",
        DecoderKind.Software => "software",
        _ => "unknown",
    };

    public static bool Matches(string? expect, DecoderKind kind) => expect switch
    {
        null => true,
        "hardware" => kind == DecoderKind.Hardware,
        _ => kind is DecoderKind.Software or DecoderKind.SoftwareTexture,
    };
}
