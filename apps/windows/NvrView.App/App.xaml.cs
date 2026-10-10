// The application: starts the core, the GPU device, the window, the render
// thread, and audio; connects the devices (from Settings, or the one given on
// the command line for development); runs the self-test when asked; and
// shuts everything down in order when the window closes.

using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using NvrView.Interop;
using Windows.Graphics;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.SWP;

namespace NvrView.App;

public partial class App : Application
{
    private Window? _window;
    private StageView? _stage;
    private StageRenderer? _renderer;
    private AudioOutput? _audio;
    private Viewer? _viewer;
    private CoreHost? _host;
    private MockNvr? _mock;
    private SelfTest? _selfTest;
    private bool _shutDown;

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) =>
        {
            AppLog.Warn($"unhandled: {e.Exception}");
            if (Program.Options.SelfTest)
            {
                SelfTest.WriteLog(Program.Options.Log!, [$"unhandled exception: {e.Exception}"], []);
                ExitCode = 1;
            }
        };
    }

    /// <summary>The process exit code (the self-test's verdict).</summary>
    public static int ExitCode { get; set; }

    internal static App Instance => (App)Current;

    internal Viewer? Viewer => _viewer;

    internal StageView? Stage => _stage;

    internal StageRenderer? Renderer => _renderer;

    internal AudioOutput? Audio => _audio;

    internal Window? MainWindow => _window;

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        AppOptions o = Program.Options;
        string dataDir = o.DataDir ?? (o.SelfTest
            ? Path.Combine(Path.GetDirectoryName(o.Log!)!, "selftest-data")
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "NvrView"));
        AppLog.Open(Path.Combine(dataDir, "logs"));
        try
        {
            Start(o, dataDir);
        }
        catch (Exception e) when (e is NvException or NvAbiMismatchException or NvrView.Video.VideoException
            or InvalidOperationException or IOException or DllNotFoundException)
        {
            AppLog.Warn($"start failed: {e}");
            if (o.SelfTest)
            {
                SelfTest.WriteLog(o.Log!, [$"start failed: {e.Message}"], []);
            }
            ExitCode = 1;
            Shutdown();
            Exit();
        }
    }

    private void Start(AppOptions o, string dataDir)
    {
        var ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        _host = new CoreHost(ui, new NvCoreOptions
        {
            LogDir = Path.Combine(dataDir, "logs", "core"),
            LogMaxFiles = 4,
            LogMaxTotalBytes = 16 << 20,
            DataDir = Path.Combine(dataDir, "core"),
        }, o.Gpu, !o.Software);
        bool memoryOnly = o.Development;
        _viewer = new Viewer(
            _host,
            memoryOnly ? new MemoryCredentials() : new WindowsCredentials(),
            new DeviceStore(memoryOnly ? null : Path.Combine(dataDir, "settings.json")));

        _window = new Window { Title = "nvrview" };
        AppWindow appWindow = _window.AppWindow;
        _stage = new StageView(_viewer, appWindow);
        _window.Content = _stage;
        _renderer = new StageRenderer(_host, _viewer, chain => ui.TryEnqueue(() => _stage.AttachSwapChain(chain)));
        _stage.Renderer = _renderer;
        _stage.SettingsRequested = () => _ = SettingsDialog.ShowAsync(_stage.XamlRoot, _viewer, memoryOnly);
        _window.Closed += (_, _) => Shutdown();

        if (o.SelfTest)
        {
            // Behind every other window, without activation: the person at the
            // computer keeps working (the self-test never takes focus).
            appWindow.MoveAndResize(new RectInt32(40, 40, 1024, 640));
            appWindow.Show(false);
            nint hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_window);
            unsafe
            {
                _ = SetWindowPos((TerraFX.Interop.Windows.HWND)hwnd, TerraFX.Interop.Windows.HWND.HWND_BOTTOM, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        else
        {
            appWindow.Resize(new SizeInt32(1280, 800));
            _window.Activate();
        }
        _stage.PushSize();
        _renderer.Start();

        if (!o.NoAudio)
        {
            _audio = new AudioOutput(_host.Core);
            _audio.Start();
        }

        if (o.MockNvr is { } mockExe)
        {
            string password = Environment.GetEnvironmentVariable("NVRVIEW_TEST_PASSWORD") is { Length: > 0 } p ? p : "example-password";
            string root = Path.GetDirectoryName(Path.GetDirectoryName(Path.GetDirectoryName(mockExe)!)!)!;
            var mockArgs = new List<string>
            {
                "--channels", o.MockChannels.ToString(System.Globalization.CultureInfo.InvariantCulture),
                "--fps", o.MockFps.ToString(System.Globalization.CultureInfo.InvariantCulture),
                "--fixture", o.MockFixture ?? Path.Combine(root, "fixtures", "synthetic", "h264-basic.h264"),
                "--audio", o.MockAudio ?? Path.Combine(root, "fixtures", "synthetic", "aac-tone.aac"),
                "--aitype", "7",
                "--detections", "3",
            };
            if (o.SelfTest && o.MockChannels >= 2)
            {
                // The last channel behaves like a third-party camera behind an
                // NVR: its first preview is refused, then it streams
                // (tile_waiting, docs/FFI.md).
                mockArgs.AddRange(["--third-party", (o.MockChannels - 1).ToString(System.Globalization.CultureInfo.InvariantCulture), "--third-party-refusals", "1"]);
            }
            _mock = MockNvr.Start(mockExe, password, mockArgs);
            _viewer.Connect(new DeviceConfig { Name = "mock-nvr", Host = _mock.Host, Port = _mock.Port, User = "example-user" }, password);
        }
        else if (o.Host is { } host)
        {
            string? password = Environment.GetEnvironmentVariable("NVRVIEW_PASSWORD");
            if (string.IsNullOrEmpty(password))
            {
                throw new InvalidOperationException("--host needs the password in NVRVIEW_PASSWORD");
            }
            _viewer.Connect(new DeviceConfig { Host = host, Port = o.Port, User = o.User }, password);
        }
        else
        {
            _viewer.LoadSavedDevices();
        }

        if (o.SelfTest)
        {
            _selfTest = new SelfTest(this, o);
            _selfTest.Start();
        }
    }

    /// <summary>Stops everything in order (idempotent): rendering, audio, tiles, devices, core, mock.</summary>
    internal void Shutdown()
    {
        if (_shutDown)
        {
            return;
        }
        _shutDown = true;
        _stage?.StopTimers();
        _renderer?.Dispose();
        _stage?.AttachSwapChain(0);
        _audio?.Dispose();
        _viewer?.CloseAll();
        _host?.Dispose();
        _mock?.Dispose();
        AppLog.Info("stopped");
    }

    /// <summary>Stops the mock-nvr (self-test: the device drops and the core reconnects).</summary>
    internal void StopMock() => _mock?.Dispose();

    /// <summary>The mock-nvr's address, when the app started one.</summary>
    internal (string Host, ushort Port)? MockAddress => _mock is { } m ? (m.Host, m.Port) : null;

    /// <summary>Ends the app (self-test).</summary>
    internal void Quit(int code)
    {
        ExitCode = code;
        Shutdown();
        _window?.Close();
        Exit();
    }
}
