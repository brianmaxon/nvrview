// The application: starts the core, the GPU device, audio, and the windows
// (restored from the last run, or one showing every camera); connects the
// devices (from Settings, or the one given on the command line for
// development); runs the self-test when asked; remembers the open windows
// (windows.json: content, frame, full screen, sidebar; no secrets); and shuts
// everything down in order when the last window closes.

using Microsoft.UI.Xaml;
using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

public partial class App : Application
{
    private readonly List<ViewerWindow> _windows = [];
    private ViewerWindow? _active;
    private AudioOutput? _audio;
    private Viewer? _viewer;
    private CoreHost? _host;
    private MockNvr? _mock;
    private SelfTest? _selfTest;
    private StartupTest? _startupTest;
    private WindowStore _windowStore = new(null);
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _saveTimer;
    private bool _shutDown;

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) =>
        {
            AppLog.Crash("unhandled exception (XAML)", e.Exception);
            if (Program.Options.SelfTest || Program.Options.StartupTest)
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

    /// <summary>The recordings model (docs/UX.md "Recordings"), shared by the Recordings window; null before start.</summary>
    internal RecordingsService? Recordings { get; private set; }

    /// <summary>Opens the Recordings window, or brings the open one forward (null when the core has no recordings).</summary>
    internal RecordingsWindow? OpenRecordings()
    {
        if (_viewer is null || _host is null || Recordings is not { Available: true } recordings)
        {
            return null;
        }
        return RecordingsWindow.Open(_viewer, _host, recordings, Program.Options.Background);
    }

    /// <summary>Self-update (docs/UPDATES.md); automatic checks are off in development and test runs.</summary>
    internal Updater? Updater { get; private set; }

    /// <summary>The open windows, the first opened first.</summary>
    internal IReadOnlyList<ViewerWindow> Windows => _windows;

    /// <summary>The viewer window the user worked in last (the first one until another is activated).</summary>
    internal ViewerWindow? ActiveViewerWindow => _active is { } a && _windows.Contains(a) ? a : _windows.FirstOrDefault();

    /// <summary>The first window's stage, renderer, and window (the self-test's main window).</summary>
    internal StageView? Stage => _windows.FirstOrDefault()?.Stage;

    internal StageRenderer? Renderer => _windows.FirstOrDefault()?.Renderer;

    internal Window? MainWindow => _windows.FirstOrDefault()?.Window;

    internal AudioOutput? Audio => _audio;

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        AppOptions o = Program.Options;
        string dataDir = o.ResolvedDataDir;
        AppLog.Open(Path.Combine(dataDir, "logs"));
        AppLog.Info($"NvrView {typeof(App).Assembly.GetName().Version?.ToString(3)}");
        if (Program.WaitedForPrevious is { } waited)
        {
            AppLog.Info(waited);
        }
        try
        {
            Start(o, dataDir);
        }
        catch (Exception e)
        {
            // Anything at all: a start that fails says why in a crash log and
            // exits, instead of dying inside XAML with nothing written.
            AppLog.Crash("start failed", e);
            if (o.SelfTest || o.StartupTest)
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
            AudioTap = true,
        }, o.Gpu, !o.Software);
        // Development modes (--host, --mock-nvr) keep nothing but the core's own store.
        bool memoryOnly = o.Development;
        _viewer = new Viewer(
            _host,
            memoryOnly ? new MemoryCredentials() : new WindowsCredentials(),
            new DeviceStore(memoryOnly ? null : Path.Combine(dataDir, "settings.json")),
            new ViewSettings(memoryOnly ? null : Path.Combine(dataDir, "view-settings.json")),
            new GroupStore(memoryOnly ? null : Path.Combine(dataDir, "groups.json")));
        _viewer.Transcripts = new TranscriptStore(Path.Combine(dataDir, "Transcripts"));
        _viewer.CallWindows = new CallWindows(_viewer, _host, () => ActiveViewerWindow, o.Background);
        // Recordings: a test run or a given data directory records into its own folder; the messages go to the windows showing the camera.
        if (o.DataDir is not null || o.SelfTest || o.StartupTest)
        {
            _viewer.Capture.FolderOverride = Path.Combine(dataDir, "Recordings");
        }
        _viewer.Capture.Notice = ShowRecordingNotice;
        _viewer.Capture.Start();
        StartRecordings(ui, o, dataDir, memoryOnly);
        _windowStore = new WindowStore(memoryOnly ? null : Path.Combine(dataDir, "windows.json"));
        Updater = new Updater(
            _viewer, ui, memoryOnly ? null : Path.Combine(dataDir, "update-settings.json"),
            automaticAllowed: !memoryOnly && !o.Background);
        _saveTimer = ui.CreateTimer();
        _saveTimer.Interval = TimeSpan.FromSeconds(1);
        _saveTimer.IsRepeating = false;
        _saveTimer.Tick += (_, _) => SaveWindows();

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
            if (o.SelfTest && o.MockChannels >= 3)
            {
                // Channel 1 is a two-lens camera (its second lens streams on mobile and extern);
                // channel 2 refuses those tiers, as a camera without a second lens does.
                mockArgs.AddRange(["--multi-lens", "1:7,7", "--extern", "2:refused"]);
            }
            if (o.SelfTest && o.MockChannels >= 3)
            {
                // Channel 2 is also a fisheye camera streaming the synthetic circular picture
                // (docs/UX.md "Fisheye cameras"); its first lens is all the dewarp step uses.
                mockArgs.AddRange(["--fisheye", "2", "--fisheye-fixture", Path.Combine(root, "fixtures", "synthetic", "fisheye", "h264-fisheye.h264"), "--fisheye-size", "384"]);
            }
            if (o.SelfTest)
            {
                // Channel 0 has a speaker (doorbell talk-back); the call steps talk to it, never to real hardware.
                mockArgs.AddRange(["--talk", "0"]);
            }
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


        // The windows of the last run, or one showing every camera (it fills
        // itself once the cameras are known).
        List<SavedWindow> restore = _windowStore.Load();
        foreach (SavedWindow w in restore)
        {
            OpenWindow(w.Content, w);
        }
        if (_windows.Count == 0)
        {
            OpenWindow(ViewerWindow.AllCameras(_viewer));
        }

        if (o.SelfTest)
        {
            _selfTest = new SelfTest(this, o);
            _selfTest.Start();
        }
        else if (o.StartupTest)
        {
            _startupTest = new StartupTest(this, o);
            _startupTest.Start();
        }
    }

    // Recordings (minor 5 and 6): configured at each launch so the cameras the user chose are searched again and
    // unfinished downloads resume; downloads go to Videos\NvrView\Incidents (Incidents in --data-dir).
    private void StartRecordings(Microsoft.UI.Dispatching.DispatcherQueue ui, AppOptions o, string dataDir, bool memoryOnly)
    {
        Recordings = new RecordingsService(
            _host!.Core, new CorePlaybackHost(_host),
            memoryOnly ? null : new RecordingsEnablementStore(Path.Combine(dataDir, "recordings-cameras.json")))
        {
            Post = a => _ = ui.TryEnqueue(() => a()),
            Trasher = path =>
            {
                if (!SystemActions.MoveToRecycleBin(path))
                {
                    AppLog.Warn("a download could not be moved to the Recycle Bin");
                }
            },
            Revealer = SystemActions.ShowInExplorer,
            CameraNamer = c => _viewer!.FindDevice(c.Device) is null ? null : _viewer.CameraName(new CameraRef(c.Device, c.Channel, c.Lens)),
        };
        _host.Recordings = Recordings;
        string downloads = o.DataDir is not null || o.SelfTest || o.StartupTest
            ? Path.Combine(dataDir, "Incidents")
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyVideos), "NvrView", "Incidents");
        Recordings.Start(downloads);
    }

    /// <summary>
    /// A message about a recording over the stage of the windows showing the camera (the viewer windows and its call window), else
    /// over the active window's.
    /// </summary>
    internal void ShowRecordingNotice(string text, CameraRef? camera, NoticeAction? action)
    {
        var stages = new List<StageView>();
        if (camera is { } c)
        {
            stages.AddRange(_windows.Select(w => w.Stage).Where(s => s.Tiles.Any(t => !t.IsEmpty && t.Camera.ChannelKey == c.ChannelKey)));
            if (_viewer?.CallWindows?.WindowFor(c) is { } callWindow)
            {
                stages.Add(callWindow.Stage);
            }
        }
        if (stages.Count == 0 && ActiveViewerWindow is { } active)
        {
            stages.Add(active.Stage);
        }
        foreach (StageView stage in stages)
        {
            stage.ShowToast(text, action);
        }
    }

    /// <summary>Opens a window with its own content (behind the others and never activated in the self-tests).</summary>
    internal ViewerWindow OpenWindow(WindowContent content, SavedWindow? restore = null)
    {
        var w = new ViewerWindow(_viewer!, _host!, content, restore, background: Program.Options.Background);
        _windows.Add(w);
        w.Changed += ScheduleSave;
        w.Closed += WindowClosed;
        w.Activated += a => _active = a;
        ScheduleSave();
        return w;
    }

    private void WindowClosed(ViewerWindow w)
    {
        if (_shutDown)
        {
            return;
        }
        if (_windows.Count == 1)
        {
            // The last window: remembered as it was, then everything stops.
            SaveWindows();
            _windows.Clear();
            Shutdown();
            Exit();
            return;
        }
        _windows.Remove(w);
        SaveWindows();
    }

    private void ScheduleSave()
    {
        if (_saveTimer is { } t && !_shutDown)
        {
            t.Stop();
            t.Start();
        }
    }

    private void SaveWindows()
    {
        try
        {
            _windowStore.Save(_windows.Select(w => w.ToSaved()));
        }
        catch (IOException e)
        {
            AppLog.Warn($"saving windows.json failed: {e.Message}");
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
        _saveTimer?.Stop();
        Updater?.Stop();
        // The Settings window goes with the app, then the call windows (their renderers and leases), then the viewer windows.
        SettingsWindow.CloseCurrent();
        RecordingsWindow.CloseCurrent();
        Recordings?.Shutdown();
        _viewer?.CallWindows?.CloseAll();
        foreach (ViewerWindow w in _windows.ToArray())
        {
            w.Close();
        }
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
        foreach (ViewerWindow w in _windows.ToArray())
        {
            w.Window.Close();
        }
        Exit();
    }
}
