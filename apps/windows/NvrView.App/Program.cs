// Entry point: parses the command line, then starts the WinUI application.
//
//   NvrView.App.exe                       devices from Settings (Credential Manager)
//   NvrView.App.exe --host H [--port N] [--user U]
//                                     one device for development; the password
//                                     comes from NVRVIEW_PASSWORD and is never
//                                     stored (use it against mock-nvr only)
//   NvrView.App.exe --mock-nvr EXE [--channels N] [--fixture F] [--audio F]
//                                     starts tools/mock-nvr on a free port and
//                                     connects to it (nothing is stored)
//   NvrView.App.exe --mock-nvr EXE --selftest SECONDS --log FILE
//                                     the self-test (SelfTest.cs): the window
//                                     opens behind the others without taking
//                                     focus, writes PASS or FAIL to FILE, exits
//
// Other options: --lib PATH (nvrview.dll), --data-dir DIR (settings, layouts,
// logs; default %LOCALAPPDATA%\NvrView), --gpu auto|hardware|warp,
// --software, --decoder-samples N and --limit-coded-size (decoder memory,
// see DecoderSettings), --no-audio.

using System.Globalization;
using System.Runtime.InteropServices;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using NvrView.Interop;
using NvrView.Video;

namespace NvrView.App;

/// <summary>The parsed command line.</summary>
internal sealed class AppOptions
{
    public string? Lib { get; private set; }

    public string? DataDir { get; private set; }

    public string? Host { get; private set; }

    public ushort Port { get; private set; } = 9000;

    public string User { get; private set; } = "example-user";

    public string? MockNvr { get; private set; }

    public int MockChannels { get; private set; } = 4;

    public string? MockFixture { get; private set; }

    public string? MockAudio { get; private set; }

    public int MockFps { get; private set; } = 20;

    public double SelfTestSeconds { get; private set; }

    public string? Log { get; private set; }

    public GpuChoice Gpu { get; private set; } = GpuChoice.Auto;

    public bool Software { get; private set; }

    public bool NoAudio { get; private set; }

    public bool CredentialTest { get; private set; }

    public bool SelfTest => SelfTestSeconds > 0;

    /// <summary>Development mode: one device given on the command line, nothing stored.</summary>
    public bool Development => Host is not null || MockNvr is not null;

    public static AppOptions Parse(string[] args)
    {
        var o = new AppOptions();
        for (int i = 0; i < args.Length; i++)
        {
            string a = args[i];
            string Value() => i + 1 < args.Length ? args[++i] : throw new ArgumentException($"{a} needs a value");
            int Int(int min, int max) =>
                int.TryParse(Value(), NumberStyles.None, CultureInfo.InvariantCulture, out int v) && v >= min && v <= max
                    ? v : throw new ArgumentException($"bad value for {a}");
            switch (a)
            {
                case "--lib": o.Lib = Path.GetFullPath(Value()); break;
                case "--data-dir": o.DataDir = Path.GetFullPath(Value()); break;
                case "--host": o.Host = Value(); break;
                case "--port": o.Port = (ushort)Int(1, 65535); break;
                case "--user": o.User = Value(); break;
                case "--mock-nvr": o.MockNvr = Path.GetFullPath(Value()); break;
                case "--channels": o.MockChannels = Int(1, 64); break;
                case "--fixture": o.MockFixture = Path.GetFullPath(Value()); break;
                case "--audio": o.MockAudio = Path.GetFullPath(Value()); break;
                case "--fps": o.MockFps = Int(1, 60); break;
                case "--selftest":
                    o.SelfTestSeconds = double.TryParse(Value(), NumberStyles.Float, CultureInfo.InvariantCulture, out double s) && s > 0
                        ? s : throw new ArgumentException("bad value for --selftest");
                    break;
                case "--log": o.Log = Path.GetFullPath(Value()); break;
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
                case "--no-audio": o.NoAudio = true; break;
                case "--credential-test": o.CredentialTest = true; break;
                case "--decoder-samples": DecoderSettings.OutputSamples = Int(0, 32); break;
                case "--limit-coded-size": DecoderSettings.LimitCodedSize = true; break;
                default: throw new ArgumentException($"unknown option {a}");
            }
        }
        if (o.SelfTest && (o.MockNvr is null || o.Log is null))
        {
            throw new ArgumentException("--selftest needs --mock-nvr EXE and --log FILE");
        }
        if (o.Host is not null && o.MockNvr is not null)
        {
            throw new ArgumentException("give --host or --mock-nvr, not both");
        }
        return o;
    }
}

internal static class Program
{
    public static AppOptions Options { get; private set; } = new();

    [STAThread]
    private static int Main(string[] args)
    {
        try
        {
            Options = AppOptions.Parse(args);
        }
        catch (ArgumentException e)
        {
            // A WinExe has no console: say it where it can be seen.
            AppLog.Startup($"nvrview: {e.Message}");
            return 2;
        }
        if (Options.Lib is { } lib)
        {
            NativeLibrary.SetDllImportResolver(typeof(NvNative).Assembly, (name, _, _) =>
                name == NvNative.Library ? NativeLibrary.Load(lib) : 0);
        }
        WinRT.ComWrappersSupport.InitializeComWrappers();
        Application.Start(p =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            _ = new App();
        });
        return App.ExitCode;
    }
}
