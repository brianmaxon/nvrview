// Live recordings and call recordings (docs/CAPTURE.md "Live recording", "Recording calls"; docs/UX.md
// "Recording"): installs the AAC encoder, configures the core from the settings (at startup and on
// every change), starts and stops recordings, and keeps the board of running ones from the core's
// recording events. The core writes the files; this only asks and shows. The macOS app's
// CaptureController does the same. UI thread only.

using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;

namespace NvrView.App;

/// <summary>A button on a message about a recording ("Show in folder", "Record Sub-stream").</summary>
internal sealed record NoticeAction(string Title, Action Handler);

internal sealed class CaptureController
{
    private readonly Viewer _viewer;
    private readonly Dictionary<CameraRef, string> _callStems = [];
    private ulong? _minFreeOverride;
    private bool _started;

    public CaptureController(Viewer viewer)
    {
        _viewer = viewer;
        Settings = new CaptureSettings(viewer.Settings.RecordingsFolder);
        viewer.Settings.Changed += Configure;
    }

    /// <summary>The running recordings and the latest ended ones.</summary>
    public RecordingBoard Board { get; } = new();

    /// <summary>The encoder handed to the core (the core keeps it until it is freed).</summary>
    public AacCaptureEncoder? Encoder { get; private set; }

    /// <summary>Whether the core took the last configuration.</summary>
    public bool Configured { get; private set; }

    /// <summary>The settings last sent to the core.</summary>
    public CaptureSettings Settings { get; private set; }

    /// <summary>A folder used instead of the setting (a test run or a given data directory records into its own folder).</summary>
    public string? FolderOverride { get; set; }

    /// <summary>The self-test's override of the free space a recording needs.</summary>
    public ulong? MinFreeOverride
    {
        get => _minFreeOverride;
        set
        {
            _minFreeOverride = value;
            Configure();
        }
    }

    /// <summary>The last refusal's message (self-test).</summary>
    public string? LastRefusal { get; private set; }

    /// <summary>Shows a message about a recording in the windows showing the camera (else the active one), with an optional button (set by the app).</summary>
    public Action<string, CameraRef?, NoticeAction?> Notice { get; set; } = (_, _, _) => { };

    // ---- setup ----

    /// <summary>Installs the encoder and configures the core (at startup, once the folder to use is known).</summary>
    public void Start()
    {
        _started = true;
        if (Encoder is null)
        {
            var encoder = new AacCaptureEncoder((recording, track, unit) => _viewer.Core.RecordAudioEncoded(recording, track, unit));
            try
            {
                _viewer.Core.CaptureSetEncoder(encoder);
                Encoder = encoder;
            }
            catch (NvException e)
            {
                AppLog.Warn($"capture: the audio encoder was not installed ({e.Status})");
            }
        }
        Configure();
    }

    /// <summary>The settings as the user has them now.</summary>
    public CaptureSettings CurrentSettings
    {
        get
        {
            ViewSettings v = _viewer.Settings;
            var s = new CaptureSettings(FolderOverride ?? v.RecordingsFolder)
            {
                Quality = v.RecordFrom,
                ShareSafe = v.ShareSafePictureNames,
                Minutes = v.MaxRecordingMinutes,
                Bytes = (ulong)Math.Round(v.MaxRecordingGB * 1e9),
            };
            return (MinFreeOverride is { } free ? s with { MinFreeBytes = free } : s).Clamped;
        }
    }

    /// <summary>Sends the settings to the core (for recordings started afterwards), when they changed.</summary>
    public void Configure()
    {
        if (!_started)
        {
            return;
        }
        CaptureSettings s = CurrentSettings;
        if (Configured && s == Settings)
        {
            return;
        }
        try
        {
            _viewer.Core.CaptureConfigure(s.Json());
            Settings = s;
            Configured = true;
        }
        catch (NvException e)
        {
            Configured = false;
            AppLog.Warn($"capture: configure failed ({e.Status})");
        }
    }

    // ---- live recordings ----

    /// <summary>Starts recording <paramref name="camera"/> (its tile's stream at <paramref name="quality"/>, else the "Record from" setting). Says why when it cannot.</summary>
    public void StartLive(CameraRef camera, RecordQuality? quality = null)
    {
        if (_viewer.Leases.Tile(camera) is not { } tile)
        {
            Refused(NvStatus.UnknownId, null, camera, call: false);
            return;
        }
        try
        {
            RecordingStarted started = _viewer.Core.RecordStart(
                RecordRequest.Live(tile.Id, RecordRequest.LocalUtcOffsetSeconds, quality?.Name(), _viewer.CameraName(camera)));
            Board.Begin(started.Recording, "live", camera, started.File, (quality ?? Settings.Quality).Name());
            _viewer.RaiseChanged();
        }
        catch (NvException e)
        {
            Refused(e.Status, e.Detail, camera, call: false);
        }
    }

    /// <summary>Record or Stop on a grid tile: stops every recording of the camera, or starts a live one.</summary>
    public void ToggleLive(CameraRef camera)
    {
        IReadOnlyList<BoardEntry> mine = Board.RecordingsOf(camera);
        if (mine.Count == 0)
        {
            StartLive(camera);
            return;
        }
        foreach (BoardEntry r in mine)
        {
            Stop(r.Id);
        }
    }

    /// <summary>Stops a recording (it finalizes in the background; its end arrives as an event).</summary>
    public void Stop(ulong recording)
    {
        try
        {
            _viewer.Core.RecordStop(recording);
        }
        catch (NvException e)
        {
            AppLog.Warn($"capture: stop {recording} failed ({e.Status})");
        }
    }

    /// <summary>Stop All Recordings.</summary>
    public void StopAll()
    {
        foreach (BoardEntry r in Board.List)
        {
            Stop(r.Id);
        }
    }

    /// <summary>Whether any recording of the camera runs (the tile's pill and its Record button).</summary>
    public bool IsRecorded(CameraRef camera) => Board.RecordingsOf(camera).Count > 0;

    // ---- call recordings ----

    /// <summary>
    /// The stem the call's recording and transcript share, reserved the first time either needs it
    /// (null without a call).
    /// </summary>
    public string? CallStem(CameraRef camera)
    {
        CameraRef key = camera.ChannelKey;
        if (_callStems.TryGetValue(key, out string? stem))
        {
            return stem;
        }
        if (_viewer.Calls.CallFor(key) is not { } call)
        {
            return null;
        }
        string name = TranscriptFiles.FileName(call.StartedAt, _viewer.CameraName(key), TimeZoneInfo.Local);
        string[] taken = Directory.Exists(_viewer.Transcripts.Folder)
            ? [.. Directory.EnumerateFiles(_viewer.Transcripts.Folder).Select(Path.GetFileName).OfType<string>()]
            : [];
        stem = CallRecordingFiles.SharedStem(name, taken);
        _callStems[key] = stem;
        return stem;
    }

    /// <summary>The stem reserved for a call, if its recording reserved one.</summary>
    public string? ReservedCallStem(CameraRef camera) => _callStems.GetValueOrDefault(camera.ChannelKey);

    /// <summary>The call ended: its stem is free again (after the transcript used it).</summary>
    public void CallEnded(CameraRef camera) => _callStems.Remove(camera.ChannelKey);

    /// <summary>Starts recording the open call of <paramref name="camera"/> beside its transcript.</summary>
    public void StartCall(CameraRef camera)
    {
        CameraRef key = camera.ChannelKey;
        if (_viewer.Calls.CallFor(key) is null || _viewer.Leases.Tile(key) is not { } tile || CallStem(key) is not { } stem)
        {
            Refused(NvStatus.UnknownId, null, camera, call: true);
            return;
        }
        string folder = _viewer.Transcripts.Folder;
        string file = Path.Combine(folder, stem + ".mp4");
        try
        {
            Directory.CreateDirectory(folder);
            RecordingStarted started = _viewer.Core.RecordStart(RecordRequest.Call(tile.Id, file));
            Board.Begin(started.Recording, "call", key, started.File, Settings.Quality.Name());
            _viewer.RaiseChanged();
            _viewer.RaiseCallsChanged();
        }
        catch (NvException e)
        {
            Refused(e.Status, e.Detail, camera, call: true);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Warn($"capture: the call recording's folder could not be made ({e.GetType().Name})");
            Refused(NvStatus.Internal, null, camera, call: true);
        }
    }

    /// <summary>The call window's Record: starts or stops the call recording.</summary>
    public void ToggleCall(CameraRef camera)
    {
        if (Board.CallOf(camera.ChannelKey) is { } r)
        {
            Stop(r.Id);
        }
        else
        {
            StartCall(camera);
        }
    }

    /// <summary>The running call recording of a doorbell, if any.</summary>
    public BoardEntry? CallRecording(CameraRef camera) => Board.CallOf(camera.ChannelKey);

    // ---- events ----

    /// <summary>A recording event from the core.</summary>
    public void Apply(RecordingEvent e)
    {
        RecordingEvent? ended = Board.Apply(e);
        _viewer.RaiseChanged();
        if (e.Kind == "call")
        {
            _viewer.RaiseCallsChanged();
        }
        if (ended is null)
        {
            return;
        }
        if (ended.Kind == "call")
        {
            _viewer.Transcripts.PruneRecordings();
        }
        AppLog.Info($"Recording {ended.State} ({ended.Reason ?? "-"}): {Path.GetFileName(ended.File)}, " +
            $"{RecordingText.Elapsed(ended.DurationMs)}, {ended.Bytes} bytes, audio {ended.Audio}");
        if (RecordingText.Ended(ended, Settings) is not { } text)
        {
            return;
        }
        DeviceEntry? device = _viewer.Devices.FirstOrDefault(d => d.Id == ended.Device);
        CameraRef? camera = device is null ? null : new CameraRef(device.Key, ended.Channel, ended.Lens);
        string file = ended.File;
        Notice(text, camera, File.Exists(file) ? new NoticeAction("Show in folder", () => SystemActions.ShowInExplorer([file])) : null);
    }

    // ---- messages ----

    private void Refused(NvStatus status, string? limit, CameraRef camera, bool call)
    {
        (string text, bool offer) = RecordingText.Refused(status, limit, Settings, call);
        LastRefusal = text;
        Notice(text, camera, offer ? new NoticeAction("Record Sub-stream", () => StartLive(camera, RecordQuality.Sub)) : null);
    }

    /// <summary>"Open recordings folder": creates it if needed and shows it in File Explorer.</summary>
    public void OpenFolder()
    {
        string folder = FolderOverride ?? _viewer.Settings.RecordingsFolder;
        try
        {
            Directory.CreateDirectory(folder);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Warn($"capture: the recordings folder could not be made ({e.GetType().Name})");
            return;
        }
        SystemActions.ShowInExplorer([folder]);
    }
}
