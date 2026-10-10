// --startup-test SECONDS: the plain start, as a double-click makes it (devices
// from Settings, the windows of the last run restored, nothing mocked), on
// the data directory given (the script passes a fresh, empty one: a first
// run). The window opens behind the others without taking focus; otherwise
// the start path is the double-click's. It checks that:
//
// 1. the main window is shown, its content loaded, and the displays were
//    read without an error;
// 2. the stage has a swap chain;
// 3. with no device configured, the stage shows its "Add a device…" prompt
//    and the sidebar its "Add a device in Settings…" link;
// 4. the app then idles for SECONDS with no unhandled exception (App's
//    handler fails the run) and no core callback exception;
// 5. the open windows are remembered in windows.json.
//
// First-chance exceptions are listed for diagnosis but do not fail the run.
// Writes what it saw and PASS or FAIL to the --log file and exits 0 or 1.

using System.Diagnostics;
using System.Runtime.ExceptionServices;
using Microsoft.UI.Xaml;

namespace NvrView.App;

internal sealed class StartupTest(App app, AppOptions options)
{
    private readonly List<string> _problems = [];
    private readonly List<string> _lines = [];
    private readonly Stopwatch _clock = Stopwatch.StartNew();
    private readonly Dictionary<string, int> _firstChance = [];
    private bool _finished;

    public void Start()
    {
        AppDomain.CurrentDomain.FirstChanceException += OnFirstChance;
        _ = RunAsync();
        _ = Watchdog();
    }

    private void OnFirstChance(object? sender, FirstChanceExceptionEventArgs e)
    {
        string key = e.Exception.GetType().Name;
        lock (_firstChance)
        {
            _firstChance[key] = _firstChance.GetValueOrDefault(key) + 1;
        }
    }

    private async Task Watchdog()
    {
        await Task.Delay(TimeSpan.FromSeconds(options.StartupTestSeconds + 60));
        _problems.Add("timed out");
        Finish();
    }

    private void Say(string line) => _lines.Add($"[{_clock.Elapsed.TotalSeconds,6:0.00}] {line}");

    private async Task RunAsync()
    {
        try
        {
            await Steps();
        }
        catch (Exception e)
        {
            _problems.Add($"startup test threw: {e}");
        }
        Finish();
    }

    private async Task Steps()
    {
        Viewer v = app.Viewer!;
        string dataDir = options.ResolvedDataDir;
        Say($"data directory has settings.json: {File.Exists(Path.Combine(dataDir, "settings.json"))}, windows.json: {File.Exists(Path.Combine(dataDir, "windows.json"))}");

        bool shown = await SelfTest.Until(() => app.Windows.Count > 0 && app.MainWindow is { } w
            && w.AppWindow.IsVisible && w.Content?.XamlRoot is not null, 15);
        if (!shown)
        {
            _problems.Add("the main window did not show");
            return;
        }
        ViewerWindow main = app.Windows[0];
        Say($"windows: {app.Windows.Count}, main window: {main.AppWindow.Size.Width}x{main.AppWindow.Size.Height}, title \"{main.Window.Title}\"");
        int screens = ViewerWindow.Screens().Count;
        Say($"displays: {screens}");
        if (screens < 1 || ViewerWindow.ScreenReadFailures > 0)
        {
            _problems.Add($"reading the displays failed ({ViewerWindow.ScreenReadFailures} times)");
        }

        if (!await SelfTest.Until(() => main.Renderer.SwapChains > 0, 10))
        {
            _problems.Add("the stage got no swap chain");
        }

        if (v.Devices.Count == 0)
        {
            bool prompt = await SelfTest.Until(() => main.Stage.ShowsAddDevicePrompt && main.Sidebar.ShowsAddDevicePrompt, 5);
            Say($"no devices: stage prompt {main.Stage.ShowsAddDevicePrompt}, sidebar link {main.Sidebar.ShowsAddDevicePrompt}");
            if (!prompt)
            {
                _problems.Add("with no devices, the \"Add a device\" prompt is not shown");
            }
        }
        else
        {
            Say($"devices: {v.Devices.Count}");
        }

        // Idle: anything unhandled meanwhile fails the run through App's handler.
        await Task.Delay(TimeSpan.FromSeconds(options.StartupTestSeconds));
        Say($"idled {options.StartupTestSeconds:0.#} s; frames presented: {main.Renderer.Frames}");
        if (v.Core.CallbackExceptionCount > 0)
        {
            _problems.Add($"callbacks threw {v.Core.CallbackExceptionCount} times: {v.Core.FirstCallbackException}");
        }
        if (!File.Exists(Path.Combine(dataDir, "windows.json")))
        {
            _problems.Add("windows.json was not written");
        }
    }

    private void Finish()
    {
        if (_finished)
        {
            return;
        }
        _finished = true;
        AppDomain.CurrentDomain.FirstChanceException -= OnFirstChance;
        lock (_firstChance)
        {
            Say(_firstChance.Count == 0
                ? "first-chance exceptions: none"
                : "first-chance exceptions: " + string.Join(", ", _firstChance.Select(p => $"{p.Key} x{p.Value}")));
        }
        SelfTest.WriteLog(options.Log!, _problems, _lines);
        app.Quit(_problems.Count == 0 ? 0 : 1);
    }
}
