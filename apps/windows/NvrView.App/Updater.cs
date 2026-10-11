// Self-update from the user's own source (docs/UPDATES.md), Windows side:
// the settings, the schedule, the core's check and download, the prompts,
// and the side-by-side switch. Nothing happens until the user sets a source
// and a key; automatic checks never open a dialog (they change the menu item
// and the Settings line); a check the user asked for answers with a dialog.

using System.Diagnostics;
using System.Text.Json;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Interop;
using NvrView.Shell;

namespace NvrView.App;

internal sealed class Updater
{
    private readonly Viewer _viewer;
    private readonly string? _file;
    private readonly DateTimeOffset _launchedAt = DateTimeOffset.Now;
    private readonly UpdateSchedule _schedule = new();
    private readonly DispatcherQueueTimer? _timer;
    private bool _userAsked;
    private string? _pending;
    private bool _dialogOpen;

    public Updater(Viewer viewer, DispatcherQueue ui, string? settingsFile, bool automaticAllowed)
    {
        _viewer = viewer;
        _file = settingsFile;
        Settings = UpdateSettings.Load(settingsFile);
        Current = typeof(Updater).Assembly.GetName().Version?.ToString(3) ?? "0.0.0";
        AppDir = AppContext.BaseDirectory;
        viewer.UpdateReceived += Handle;
        // Only where an earlier download made the folder: with updates off,
        // nothing is created next to the app folder.
        SideBySide.RemoveLeftovers(SideBySide.FolderPath(AppDir));
        if (automaticAllowed)
        {
            _timer = ui.CreateTimer();
            _timer.Interval = TimeSpan.FromMinutes(5);
            _timer.IsRepeating = true;
            _timer.Tick += (_, _) => Poll();
            _timer.Start();
            DispatcherQueueTimer first = ui.CreateTimer();
            first.Interval = _schedule.StartupDelay + TimeSpan.FromSeconds(1);
            first.IsRepeating = false;
            first.Tick += (_, _) => Poll();
            first.Start();
        }
    }

    public UpdateSettings Settings { get; }

    public UpdateStatus Status { get; private set; } = UpdateStatus.Idle;

    public string Current { get; }

    /// <summary>The folder this copy of the app runs from.</summary>
    public string AppDir { get; }

    public event Action? Changed;

    public string MenuTitle => Status.State switch
    {
        UpdateStatus.Kind.Available => $"Update to {Status.Version}…",
        UpdateStatus.Kind.Ready => $"Switch to {Status.Version}…",
        _ => "Check for updates…",
    };

    public void Stop() => _timer?.Stop();

    public void Poll()
    {
        if (Status.State is UpdateStatus.Kind.Checking or UpdateStatus.Kind.Downloading or UpdateStatus.Kind.Ready)
        {
            return;
        }
        if (_schedule.IsDue(Settings, DateTimeOffset.Now, _launchedAt))
        {
            Check(userAsked: false);
        }
    }

    // ---- settings ----

    public string? SetSource(string text)
    {
        string t = text.Trim();
        if (t.Length > 0)
        {
            using JsonDocument d = JsonDocument.Parse(NvCore.UpdateParseSource(t));
            if (!d.RootElement.GetProperty("ok").GetBoolean())
            {
                return d.RootElement.TryGetProperty("message", out JsonElement m) ? m.GetString() : "Not a usable source.";
            }
        }
        Settings.Source = t;
        Save();
        return null;
    }

    public string? AddKey(string text)
    {
        if (NvCore.UpdateParseKey(text) is not { } json)
        {
            return "That is not an NvrView update public key.";
        }
        using JsonDocument d = JsonDocument.Parse(json);
        string key = d.RootElement.GetProperty("key").GetString()!;
        if (!Settings.Keys.Contains(key))
        {
            Settings.Keys.Add(key);
            Save();
        }
        return null;
    }

    public void RemoveKey(string key)
    {
        Settings.Keys.Remove(key);
        Save();
    }

    public static string KeyId(string key)
    {
        if (NvCore.UpdateParseKey(key) is not { } json)
        {
            return "?";
        }
        using JsonDocument d = JsonDocument.Parse(json);
        return d.RootElement.GetProperty("id").GetString() ?? "?";
    }

    public void SetAutomatic(bool on)
    {
        Settings.Automatic = on;
        Save();
    }

    private void Save()
    {
        try
        {
            Settings.Save(_file);
        }
        catch (IOException e)
        {
            AppLog.Warn($"saving update-settings.json failed: {e.Message}");
        }
        Changed?.Invoke();
    }

    // ---- checking, downloading, switching ----

    public void MenuAction()
    {
        switch (Status.State)
        {
            case UpdateStatus.Kind.Available: _ = AskDownload(Status.Version, Status.Notes); break;
            case UpdateStatus.Kind.Ready: _ = AskSwitch(Status.Version); break;
            case UpdateStatus.Kind.Checking or UpdateStatus.Kind.Downloading: break;
            default: Check(userAsked: true); break;
        }
    }

    public void Check(bool userAsked)
    {
        if (!Settings.CanCheck)
        {
            if (userAsked)
            {
                _ = Tell("Updates", "Set an update source and add a trusted key in Settings first.");
            }
            return;
        }
        _userAsked = userAsked;
        try
        {
            _viewer.Core.UpdateCheck(UpdateRequest.Json(Settings, Current));
            SetStatus(new UpdateStatus(UpdateStatus.Kind.Checking));
        }
        catch (NvException e) when (e.Status == NvStatus.Limit)
        {
            // Already checking or downloading.
        }
        catch (NvException e)
        {
            AppLog.Warn($"update check refused: {e.Status}");
            SetStatus(new UpdateStatus(UpdateStatus.Kind.Failed, Message: "The update check could not start."));
        }
    }

    private void Download(string version)
    {
        if (SideBySide.UpdateFolder(AppDir, out string? problem) is not { } folder)
        {
            SetStatus(new UpdateStatus(UpdateStatus.Kind.Failed, Message: problem!));
            _ = Tell("Update", problem!);
            return;
        }
        _pending = version;
        _userAsked = true;
        try
        {
            _viewer.Core.UpdateDownload(UpdateRequest.Json(Settings, Current, version, SideBySide.NewStagingPath(folder, version)));
            SetStatus(new UpdateStatus(UpdateStatus.Kind.Downloading, version));
        }
        catch (NvException e)
        {
            AppLog.Warn($"update download refused: {e.Status}");
            SetStatus(new UpdateStatus(UpdateStatus.Kind.Failed, Message: "The download could not start."));
        }
    }

    private void Handle(UpdateEvent e)
    {
        if (UpdateStatus.After(e, DateTimeOffset.Now, _pending) is not { } next)
        {
            return;
        }
        if (e.IsFinal && e.Action == "check")
        {
            Settings.LastCheck = DateTimeOffset.Now;
            Save();
        }
        // An update installed earlier (Later) is offered again without a download.
        if (next.State == UpdateStatus.Kind.Available && SideBySide.FindInstalled(AppDir, next.Version) is { } there)
        {
            next = new UpdateStatus(UpdateStatus.Kind.Ready, next.Version, Path: there);
        }
        if (next.State == UpdateStatus.Kind.Ready && e.State == "ready")
        {
            try
            {
                string installed = SideBySide.Install(next.Path, AppDir, next.Version);
                next = next with { Path = installed };
                AppLog.Info($"update {next.Version} installed beside this version");
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or InvalidDataException)
            {
                AppLog.Warn($"installing the update failed: {ex.Message}");
                next = new UpdateStatus(UpdateStatus.Kind.Failed, Message: $"The update could not be installed beside this version: {ex.Message}");
            }
        }
        SetStatus(next);
        if (!e.IsFinal)
        {
            return;
        }
        bool asked = _userAsked;
        _userAsked = false;
        switch (next.State)
        {
            case UpdateStatus.Kind.Available when asked: _ = AskDownload(next.Version, next.Notes); break;
            case UpdateStatus.Kind.Ready when asked || e.State == "ready": _ = AskSwitch(next.Version); break;
            case UpdateStatus.Kind.UpToDate when asked: _ = Tell("NvrView is up to date", $"Version {Current} is the newest at your update source."); break;
            case UpdateStatus.Kind.Failed when asked: _ = Tell("Update", next.Message); break;
        }
    }

    private void SetStatus(UpdateStatus s)
    {
        Status = s;
        Changed?.Invoke();
    }

    /// <summary>Restart now: start the new version (it waits for this process to exit) and quit.</summary>
    private void SwitchNow()
    {
        string exe = Path.Combine(Status.Path, SideBySide.Executable);
        try
        {
            var start = new ProcessStartInfo(exe) { WorkingDirectory = Status.Path, UseShellExecute = false };
            start.ArgumentList.Add("--wait-for-pid");
            start.ArgumentList.Add(Environment.ProcessId.ToString(System.Globalization.CultureInfo.InvariantCulture));
            Process.Start(start)?.Dispose();
            AppLog.Info($"switching to {Status.Version}");
            App.Instance.Quit(0);
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or IOException)
        {
            AppLog.Warn($"starting the new version failed: {e.Message}");
            _ = Tell("Update", $"The new version could not be started: {e.Message}");
        }
    }

    // ---- dialogs ----

    private async Task<ContentDialogResult?> Show(string title, string text, string? primary, string close)
    {
        if (Program.Options.Background || _dialogOpen || App.Instance.MainWindow?.Content?.XamlRoot is not { } root)
        {
            return null;
        }
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = title,
            Content = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, MaxWidth = 420 },
            CloseButtonText = close,
            DefaultButton = primary is null ? ContentDialogButton.Close : ContentDialogButton.Primary,
        };
        if (primary is not null)
        {
            dialog.PrimaryButtonText = primary;
        }
        _dialogOpen = true;
        try
        {
            return await dialog.ShowAsync();
        }
        catch (Exception e) when (e is InvalidOperationException or System.Runtime.InteropServices.COMException)
        {
            // Another dialog is open: the menu item still offers it.
            return null;
        }
        finally
        {
            _dialogOpen = false;
        }
    }

    private Task Tell(string title, string text) => Show(title, text, null, "OK");

    private async Task AskDownload(string version, string notes)
    {
        string text = (notes.Length > 0 ? notes + "\n\n" : "") + $"You have {Current}. The new version is installed in its own folder next to this one; this one stays as it is.";
        if (await Show($"NvrView {version} is available", text, "Download", "Not now") == ContentDialogResult.Primary)
        {
            Download(version);
        }
    }

    private async Task AskSwitch(string version)
    {
        string text = $"NvrView {version} is ready in {Status.Path}. Restart now to switch; with Later, it is offered again next time. Shortcuts still open this version until you point them at the new folder.";
        if (await Show($"Update to {version}?", text, "Restart now", "Later") == ContentDialogResult.Primary)
        {
            SwitchNow();
        }
    }
}
