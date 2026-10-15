// Which recognizer a call's captions use on Windows (docs/ARCHITECTURE.md "Live transcription"):
// Moonshine streaming when its download is installed, else the desktop recognizer of Windows
// (SapiTranscriber), which also takes over a session whose model fails to load. The download
// (CaptionModel.cs) starts on first use, in the background, from the update source; the call that
// started it captions with the desktop recognizer, and the calls after it use Moonshine. Both
// recognize English, as the Mac's does. Nothing here is shown in the call: the Mac names no engine
// either; the choice and the download's progress go to the log.

using System.Reflection;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>Makes each caption session's recognizer; <paramref name="desktop"/> makes the desktop recognizer (a fake in the self-test).</summary>
internal sealed class CaptionEngines(CaptionModelStore? store, Func<string> updateSource, Func<ITranscriptionEngine>? desktop = null)
{
    private readonly Func<ITranscriptionEngine> _desktop = desktop ?? (() => new SapiTranscriber());
    private readonly Lock _gate = new();
    private Task? _install;
    private bool _logged;

    /// <summary>The engines of the running app: the manifest embedded at build time, the Models folder in the data directory.</summary>
    public static CaptionEngines Real(string dataDir) =>
        new(Manifest() is { } m ? new CaptionModelStore(m, Path.Combine(dataDir, "Models")) : null,
            () => App.Instance.Updater?.Settings.EffectiveSource ?? "");

    /// <summary>scripts/caption-model.json as this build embedded it, or null.</summary>
    public static CaptionModelManifest? Manifest()
    {
        using Stream? s = Assembly.GetExecutingAssembly().GetManifestResourceStream(CaptionModelManifest.ResourceName);
        return s is null ? null : CaptionModelManifest.Parse(new StreamReader(s).ReadToEnd());
    }

    /// <summary>The completed install, for the self-test; null until one ran.</summary>
    public Task? Install => _install;

    /// <summary>A recognizer for one source of a call.</summary>
    public ITranscriptionEngine Make()
    {
        if (store?.Locate() is { } folder)
        {
            Log($"captions: Moonshine ({store.Manifest.Name})");
            return new MoonshineTranscriber(folder, store.Manifest.Arch, _desktop);
        }
        StartInstall();
        return _desktop();
    }

    // Once per run: a failed or impossible install is logged and not retried until the app starts again.
    private void StartInstall()
    {
        lock (_gate)
        {
            if (_install is not null || store is null)
            {
                return;
            }
            string source = updateSource();
            _install = Task.Run(async () =>
            {
                CaptionModelStatus s = await store.EnsureAsync(source).ConfigureAwait(false);
                AppLog.Info($"captions: {s.Text}; {(s.IsReady ? "calls from now on use Moonshine" : "calls use the desktop recognizer of Windows")}");
            });
        }
    }

    private void Log(string line)
    {
        if (!_logged)
        {
            _logged = true;
            AppLog.Info(line);
        }
    }
}
