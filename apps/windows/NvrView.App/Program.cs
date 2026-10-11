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
//   NvrView.App.exe --startup-test SECONDS --log FILE [--data-dir DIR]
//                                     the plain start (a double-click: devices
//                                     from Settings, windows restored) on DIR
//                                     (default: a fresh folder beside FILE);
//                                     checks the window shows and the app idles
//                                     without an exception, writes PASS or FAIL
//                                     to FILE, exits (StartupTest.cs)
//
// A crash (an exception nothing handled) is written to crash-*.log in the
// logs directory (AppLog.Crash) before the process ends.
//
// After an update the new version is started with --wait-for-pid PID: it
// waits (at most 2 minutes) for the previous version to exit before it logs in
// to any device (docs/UPDATES.md, "Applying", Windows).
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

    public double StartupTestSeconds { get; private set; }

    public string? Log { get; private set; }

    public GpuChoice Gpu { get; private set; } = GpuChoice.Auto;

    public bool Software { get; private set; }

    public bool NoAudio { get; private set; }

    public bool CredentialTest { get; private set; }

    /// <summary>Started by an update: wait for the previous version (this process ID) to exit first.</summary>
    public int? WaitForPid { get; private set; }

    /// <summary>The self-test also opens the microphone for a moment (the audio is measured and dropped).</summary>
    public bool MicrophoneTest { get; private set; }

    public bool SelfTest => SelfTestSeconds > 0;

    public bool StartupTest => StartupTestSeconds > 0;

    /// <summary>A test run: windows open behind the others and are never activated.</summary>
    public bool Background => SelfTest || StartupTest;

    /// <summary>The data directory (settings, layouts, logs).</summary>
    public string ResolvedDataDir => DataDir ?? (SelfTest
        ? Path.Combine(Path.GetDirectoryName(Log!)!, "selftest-data")
        : StartupTest
            ? Path.Combine(Path.GetDirectoryName(Log!)!, "startup-data")
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "NvrView"));

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
                case "--startup-test":
                    o.StartupTestSeconds = double.TryParse(Value(), NumberStyles.Float, CultureInfo.InvariantCulture, out double t) && t > 0
                        ? t : throw new ArgumentException("bad value for --startup-test");
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
                case "--wait-for-pid": o.WaitForPid = Int(1, int.MaxValue); break;
                case "--mic-test": o.MicrophoneTest = true; break;
                case "--decoder-samples": DecoderSettings.OutputSamples = Int(0, 32); break;
                case "--limit-coded-size": DecoderSettings.LimitCodedSize = true; break;
                default: throw new ArgumentException($"unknown option {a}");
            }
        }
        if (o.SelfTest && (o.MockNvr is null || o.Log is null))
        {
            throw new ArgumentException("--selftest needs --mock-nvr EXE and --log FILE");
        }
        if (o.StartupTest && (o.Log is null || o.SelfTest || o.Development))
        {
            throw new ArgumentException("--startup-test needs --log FILE, and no --selftest, --mock-nvr, or --host");
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

    /// <summary>What --wait-for-pid found, logged once the log is open.</summary>
    public static string? WaitedForPrevious { get; private set; }

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
        if (Options.WaitForPid is { } pid)
        {
            var waited = System.Diagnostics.Stopwatch.StartNew();
            bool exited = NvrView.Shell.SideBySide.WaitForExit(pid, TimeSpan.FromMinutes(2));
            WaitedForPrevious = $"started after an update: the previous version {(exited ? "exited" : "was still running")} after {waited.ElapsedMilliseconds} ms";
        }
        if (Options.Lib is { } lib)
        {
            NativeLibrary.SetDllImportResolver(typeof(NvNative).Assembly, (name, _, _) =>
                name == NvNative.Library ? NativeLibrary.Load(lib) : 0);
        }
        // Whatever nothing else handles is written to a crash log first.
        string crashDir = Path.Combine(Options.ResolvedDataDir, "logs");
        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
        {
            if (e.ExceptionObject is Exception ex)
            {
                AppLog.Crash("unhandled exception (AppDomain)", ex, crashDir);
            }
        };
        TaskScheduler.UnobservedTaskException += (_, e) =>
        {
            AppLog.Crash("unobserved task exception", e.Exception, crashDir);
            e.SetObserved();
        };
        try
        {
            WinRT.ComWrappersSupport.InitializeComWrappers();
            Application.Start(p =>
            {
                var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
                SynchronizationContext.SetSynchronizationContext(context);
                _ = new App();
            });
        }
        catch (Exception e)
        {
            AppLog.Crash("startup failed", e, crashDir);
            return 1;
        }
        return App.ExitCode;
    }
}
