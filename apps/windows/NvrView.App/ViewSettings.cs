// View preferences kept between launches in view-settings.json in the app's
// data directory (separate from the device list in settings.json): today the
// corner that holds the tiles' indicators, the corner call cards stack from,
// and whether the sidebar hides
// offline channels, and the mount of each fisheye camera (docs/UX.md
// "Fisheye cameras"; only those not on the default ceiling mount). Written
// whole and atomically; the cameras the user allowed talk-back to (docs/UX.md "Doorbell calls");
// the call window's settings (docs/UX.md "A call in its own window"): whether calls open in their
// own window, each doorbell's own choice, where call windows open, the frame the user left one at,
// and its pin; the Settings window's frame; unreadable or missing reads as the defaults. Memory only when file is null.

using System.Text.Json;
using System.Text.Json.Serialization;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

internal sealed class ViewSettingsFile
{
    public int Version { get; set; } = 1;

    /// <summary>topLeft, topRight (default), bottomLeft, bottomRight.</summary>
    public string IndicatorCorner { get; set; } = ChromeCorners.Default.Name();

    /// <summary>The sidebar lists only connected channels (an NVR lists every slot it has).</summary>
    public bool HideOfflineChannels { get; set; } = true;

    /// <summary>The corner call cards stack from (a corner name); absent: opposite the indicators.</summary>
    public string? InsetCorner { get; set; }

    /// <summary>Cameras marked as entrances ("deviceKey:channel"): a person on them opens an alert inset; off unless listed.</summary>
    public List<string>? EntranceCameras { get; set; }

    /// <summary>The Settings default for how many alert insets a window draws: rows per column; absent: 3.</summary>
    public int? InsetRows { get; set; }

    /// <summary>The Settings default for how many alert insets a window draws: columns; absent: 1.</summary>
    public int? InsetColumns { get; set; }

    /// <summary>Fisheye mounts other than ceiling, by camera ("deviceKey:channel"): "wall" or "desk".</summary>
    public Dictionary<string, string>? FisheyeMounts { get; set; }

    /// <summary>Cameras the user allowed talk-back to ("deviceKey:channel"); camera motion control, off unless listed.</summary>
    public List<string>? TalkAllowed { get; set; }

    /// <summary>Automations the user let move a camera, by camera ("deviceKey:channel"): client names such as auto_correction. Camera motion control, off unless listed; the user's own PTZ controls need no entry.</summary>
    public Dictionary<string, string[]>? MotionGrants { get; set; }

    /// <summary>The pan and tilt pad's speed (1 to 64); absent: 20.</summary>
    public int? PtzSpeed { get; set; }

    /// <summary>Doorbells whose ring unmutes them even when the user is not listening ("deviceKey:channel"); off unless listed.</summary>
    public List<string>? UnmuteOnRing { get; set; }

    /// <summary>"Open calls in their own window" (docs/UX.md "A call in its own window"); off by default.</summary>
    public bool CallsOpenInWindow { get; set; }

    /// <summary>Doorbells whose calls open where the user chose, by camera ("deviceKey:channel"): "window" or "inset"; absent: the app's setting.</summary>
    public Dictionary<string, string>? CallPresentation { get; set; }

    /// <summary>"Open call windows": absent for where the user left them (default), or activeScreen.</summary>
    public string? CallWindowPlacement { get; set; }

    /// <summary>The frame the user last left a call window at, in physical pixels: x, y, width, height.</summary>
    public double[]? CallWindowFrame { get; set; }

    /// <summary>The frame the user last left the Settings window at, in physical pixels: x, y, width, height.</summary>
    public double[]? SettingsWindowFrame { get; set; }

    /// <summary>The call window's pin (always on top); absent: on.</summary>
    public bool? CallWindowOnTop { get; set; }

    /// <summary>Call transcripts go into the log and, one text file per call, the Transcripts folder (off by default).</summary>
    public bool SaveCallTranscripts { get; set; }

    /// <summary>Where Save Picture writes (docs/UX.md "Save picture"); absent: Pictures\NvrView.</summary>
    public string? PicturesFolder { get; set; }

    /// <summary>The picture format: absent for png (the default), or jpeg.</summary>
    public string? PictureFormat { get; set; }

    /// <summary>Saved pictures name the camera "Camera A" (off by default).</summary>
    public bool ShareSafePictureNames { get; set; }

    /// <summary>"Export from" (the Recordings window's Export tab): absent for the main stream, or sub.</summary>
    public string? ExportQuality { get; set; }

    /// <summary>"Save recordings to" (docs/CAPTURE.md "Live recording"); absent: the Videos folder's nvrview folder.</summary>
    public string? RecordingsFolder { get; set; }

    /// <summary>"Record from": main (default) or sub.</summary>
    public string? RecordFrom { get; set; }

    /// <summary>"Stop after" in minutes; absent: 60.</summary>
    public int? MaxRecordingMinutes { get; set; }

    /// <summary>"or at" in gigabytes (decimal); absent: 4.</summary>
    public double? MaxRecordingGB { get; set; }

    /// <summary>"Record calls": a doorbell call is recorded from the moment it opens (off by default).</summary>
    public bool RecordCalls { get; set; }

    /// <summary>The consent note was shown (the first time "Record calls" is turned on).</summary>
    public bool RecordCallsConsentShown { get; set; }
}

[JsonSerializable(typeof(ViewSettingsFile))]
[JsonSourceGenerationOptions(WriteIndented = true, PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
internal sealed partial class ViewSettingsJson : JsonSerializerContext;

internal sealed class ViewSettings(string? file)
{
    private ChromeCorner _corner = ChromeCorners.Default;
    private ChromeCorner? _insetCorner;
    private bool _hideOffline = true;
    private bool _saveTranscripts;
    private string? _picturesFolder;
    private PictureFormat _pictureFormat = PictureFormat.Png;
    private bool _shareSafeNames;
    private ExportQuality _exportQuality = ExportQuality.Main;
    private bool _loaded;
    private Dictionary<string, string> _mounts = [];
    private HashSet<string> _talkAllowed = [];
    private Dictionary<string, HashSet<string>> _motionGrants = [];
    private int _ptzSpeed;
    private HashSet<string> _unmuteOnRing = [];
    private HashSet<string> _entrance = [];
    private InsetGridSetting _insetGrid = InsetGridSetting.Standard;
    private bool _callsInWindow;
    private Dictionary<string, string> _callPresentations = [];
    private CallWindowPlacement.Mode _placement = CallWindowPlacement.Mode.Remembered;
    private WindowRect? _callFrame;
    private WindowRect? _settingsFrame;
    private bool _callOnTop = true;
    private string? _recordingsFolder;
    private RecordQuality _recordFrom = RecordQuality.Main;
    private int _maxRecordingMinutes = 60;
    private double _maxRecordingGB = 4;
    private bool _recordCalls;
    private bool _recordCallsConsent;

    private static string MountKey(CameraRef camera) => $"{camera.Device}:{camera.Channel}";

    /// <summary>A fisheye camera's mount (a channel's, whatever the lens); ceiling unless set.</summary>
    public FisheyeMount Mount(CameraRef camera)
    {
        Load();
        return _mounts.TryGetValue(MountKey(camera), out string? m) ? Fisheye.MountFromName(m) : FisheyeMount.Ceiling;
    }

    /// <summary>Sets a fisheye camera's mount (saved; ceiling removes the entry).</summary>
    public void SetMount(CameraRef camera, FisheyeMount mount)
    {
        Load();
        if (Mount(camera) == mount)
        {
            return;
        }
        if (mount == FisheyeMount.Ceiling)
        {
            _mounts.Remove(MountKey(camera));
        }
        else
        {
            _mounts[MountKey(camera)] = mount.Name();
        }
        Save();
        Changed?.Invoke();
    }

    /// <summary>Talk-back to a camera (a channel's, whatever the lens) is allowed. Camera motion control: off unless the user turned it on.</summary>
    public bool TalkAllowed(CameraRef camera)
    {
        Load();
        return _talkAllowed.Contains(MountKey(camera));
    }

    /// <summary>Allows or forbids talk-back to a camera (saved).</summary>
    public void SetTalkAllowed(CameraRef camera, bool allowed)
    {
        Load();
        if (allowed ? !_talkAllowed.Add(MountKey(camera)) : !_talkAllowed.Remove(MountKey(camera)))
        {
            return;
        }
        Save();
        Changed?.Invoke();
    }

    /// <summary>The automations the user let move a camera (a channel's, whatever the lens); empty unless allowed.</summary>
    public IReadOnlyCollection<string> MotionGrants(CameraRef camera)
    {
        Load();
        return _motionGrants.TryGetValue(MountKey(camera), out HashSet<string>? g) ? [.. g.Order(StringComparer.Ordinal)] : [];
    }

    /// <summary>Every grant of a device's cameras: the channel and the automation.</summary>
    public IReadOnlyList<(int Channel, string Client)> MotionGrantsFor(string deviceKey)
    {
        Load();
        var result = new List<(int, string)>();
        string prefix = deviceKey + ":";
        foreach ((string key, HashSet<string> clients) in _motionGrants)
        {
            if (key.StartsWith(prefix, StringComparison.Ordinal) && int.TryParse(key.AsSpan(prefix.Length), out int channel))
            {
                result.AddRange(clients.Order(StringComparer.Ordinal).Select(c => (channel, c)));
            }
        }
        return result;
    }

    /// <summary>Whether the automation may move the camera.</summary>
    public bool MotionGranted(CameraRef camera, string client)
    {
        Load();
        return _motionGrants.TryGetValue(MountKey(camera), out HashSet<string>? g) && g.Contains(client);
    }

    /// <summary>Lets (or stops letting) an automation move a camera (saved).</summary>
    public void SetMotionGrant(CameraRef camera, string client, bool granted)
    {
        Load();
        string key = MountKey(camera);
        if (!_motionGrants.TryGetValue(key, out HashSet<string>? g))
        {
            g = [];
            _motionGrants[key] = g;
        }
        bool changed = granted ? g.Add(client) : g.Remove(client);
        if (g.Count == 0)
        {
            _motionGrants.Remove(key);
        }
        if (!changed)
        {
            return;
        }
        Save();
        Changed?.Invoke();
    }

    /// <summary>The pad's speed kept from the last run (0: none kept).</summary>
    public int PtzSpeed
    {
        get
        {
            Load();
            return _ptzSpeed;
        }
        set
        {
            Load();
            if (_ptzSpeed == value)
            {
                return;
            }
            _ptzSpeed = value;
            Save();
        }
    }

    /// <summary>A ring of this doorbell (a channel's, whatever the lens) plays its sound even when the user is not listening to anything.</summary>
    public bool UnmuteOnRing(CameraRef camera)
    {
        Load();
        return _unmuteOnRing.Contains(MountKey(camera));
    }

    /// <summary>Turns "unmute on ring" on or off for a doorbell (saved).</summary>
    public void SetUnmuteOnRing(CameraRef camera, bool on)
    {
        Load();
        if (on ? !_unmuteOnRing.Add(MountKey(camera)) : !_unmuteOnRing.Remove(MountKey(camera)))
        {
            return;
        }
        Save();
        Changed?.Invoke();
    }

    /// <summary>The camera (a channel's, whatever the lens) is marked as an entrance: a person detected on it opens an alert inset when the camera is not on screen.</summary>
    public bool Entrance(CameraRef camera)
    {
        Load();
        return _entrance.Contains(MountKey(camera));
    }

    /// <summary>Marks or unmarks a camera as an entrance (saved).</summary>
    public void SetEntrance(CameraRef camera, bool on)
    {
        Load();
        if (on ? !_entrance.Add(MountKey(camera)) : !_entrance.Remove(MountKey(camera)))
        {
            return;
        }
        Save();
        Changed?.Invoke();
    }

    /// <summary>How many alert insets a window draws (rows x columns) unless the window has its own grid.</summary>
    public InsetGridSetting InsetGrid
    {
        get
        {
            Load();
            return _insetGrid;
        }
        set
        {
            Load();
            if (_insetGrid == value)
            {
                return;
            }
            _insetGrid = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>"Open calls in their own window": a ring or Start Conversation opens the call in a call window (a doorbell's own choice wins).</summary>
    public bool CallsOpenInWindow
    {
        get
        {
            Load();
            return _callsInWindow;
        }
        set
        {
            Load();
            if (_callsInWindow == value)
            {
                return;
            }
            _callsInWindow = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>The doorbell's own choice of where its calls open, null for Default (the setting above decides).</summary>
    public CallPresentation? CallPresentationFor(CameraRef camera)
    {
        Load();
        return _callPresentations.TryGetValue(MountKey(camera), out string? p) ? (p == "window" ? CallPresentation.Window : CallPresentation.Inset) : null;
    }

    /// <summary>Sets the doorbell's choice (saved); null is Default and removes the entry.</summary>
    public void SetCallPresentation(CameraRef camera, CallPresentation? presentation)
    {
        Load();
        if (CallPresentationFor(camera) == presentation)
        {
            return;
        }
        if (presentation is null)
        {
            _callPresentations.Remove(MountKey(camera));
        }
        else
        {
            _callPresentations[MountKey(camera)] = presentation == CallPresentation.Window ? "window" : "inset";
        }
        Save();
        Changed?.Invoke();
    }

    /// <summary>"Open call windows": where the user left them, or the top right of the active screen.</summary>
    public CallWindowPlacement.Mode CallWindowPlacementMode
    {
        get
        {
            Load();
            return _placement;
        }
        set
        {
            Load();
            if (_placement == value)
            {
                return;
            }
            _placement = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>The frame the user last left a call window at (physical pixels, virtual-screen origin); saved without raising Changed (the app debounces moves).</summary>
    public WindowRect? CallWindowFrame
    {
        get
        {
            Load();
            return _callFrame;
        }
        set
        {
            Load();
            if (_callFrame == value)
            {
                return;
            }
            _callFrame = value;
            Save();
        }
    }

    /// <summary>The frame the user last left the Settings window at (physical pixels, virtual-screen origin); saved without raising Changed.</summary>
    public WindowRect? SettingsWindowFrame
    {
        get
        {
            Load();
            return _settingsFrame;
        }
        set
        {
            Load();
            if (_settingsFrame == value)
            {
                return;
            }
            _settingsFrame = value;
            Save();
        }
    }

    /// <summary>The call window's pin: on top of other windows (on by default).</summary>
    public bool CallWindowOnTop
    {
        get
        {
            Load();
            return _callOnTop;
        }
        set
        {
            Load();
            if (_callOnTop == value)
            {
                return;
            }
            _callOnTop = value;
            Save();
        }
    }

    /// <summary>The sidebar lists only connected channels.</summary>
    public bool HideOfflineChannels
    {
        get
        {
            Load();
            return _hideOffline;
        }
        set
        {
            Load();
            if (_hideOffline == value)
            {
                return;
            }
            _hideOffline = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>
    /// "Save call transcripts" (docs/UX.md "Doorbell calls"): each call's transcript goes into the
    /// log and into a text file in the Transcripts folder. Off by default (recording a conversation
    /// may need consent).
    /// </summary>
    public bool SaveCallTranscripts
    {
        get
        {
            Load();
            return _saveTranscripts;
        }
        set
        {
            Load();
            if (_saveTranscripts == value)
            {
                return;
            }
            _saveTranscripts = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>Pictures\NvrView in the user's Pictures folder: where Save Picture writes unless told otherwise.</summary>
    public static string DefaultPicturesFolder =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyPictures), "NvrView");

    /// <summary>Where Save Picture writes (Settings > Capture); the default folder unless chosen.</summary>
    public string PicturesFolder
    {
        get
        {
            Load();
            return _picturesFolder ?? DefaultPicturesFolder;
        }
        set
        {
            Load();
            string? chosen = string.IsNullOrWhiteSpace(value) || value == DefaultPicturesFolder ? null : value;
            if (_picturesFolder == chosen)
            {
                return;
            }
            _picturesFolder = chosen;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>The format Save Picture writes (PNG unless changed).</summary>
    public PictureFormat PictureFormat
    {
        get
        {
            Load();
            return _pictureFormat;
        }
        set
        {
            Load();
            if (_pictureFormat == value)
            {
                return;
            }
            _pictureFormat = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>"Export from": the stream a download and an export read until the Export tab says otherwise (the main stream unless changed).</summary>
    public ExportQuality ExportQuality
    {
        get
        {
            Load();
            return _exportQuality;
        }
        set
        {
            Load();
            if (_exportQuality == value)
            {
                return;
            }
            _exportQuality = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>Saved pictures name the camera "Camera A" instead of its own name.</summary>
    public bool ShareSafePictureNames
    {
        get
        {
            Load();
            return _shareSafeNames;
        }
        set
        {
            Load();
            if (_shareSafeNames == value)
            {
                return;
            }
            _shareSafeNames = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>Where this computer's recordings go; the Videos folder's "nvrview" folder until the user chooses (docs/CAPTURE.md "Live recording").</summary>
    public string RecordingsFolder
    {
        get
        {
            Load();
            return _recordingsFolder is { Length: > 0 } f && Path.IsPathFullyQualified(f) ? f : DefaultRecordingsFolder;
        }
        set
        {
            Load();
            string? v = value == DefaultRecordingsFolder || value.Length == 0 ? null : value;
            if (_recordingsFolder == v)
            {
                return;
            }
            _recordingsFolder = v;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>The Videos folder's nvrview folder.</summary>
    public static string DefaultRecordingsFolder =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyVideos) is { Length: > 0 } v ? v
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Videos"), "nvrview");

    /// <summary>"Record from": the stream a recording takes.</summary>
    public RecordQuality RecordFrom
    {
        get
        {
            Load();
            return _recordFrom;
        }
        set => Change(ref _recordFrom, value);
    }

    /// <summary>"Stop after", in minutes (1 to 720).</summary>
    public int MaxRecordingMinutes
    {
        get
        {
            Load();
            return _maxRecordingMinutes;
        }
        set => Change(ref _maxRecordingMinutes, Math.Clamp(value, CaptureSettings.MinMinutes, CaptureSettings.MaxMinutes));
    }

    /// <summary>"or at", in gigabytes (0.1 to 64).</summary>
    public double MaxRecordingGB
    {
        get
        {
            Load();
            return _maxRecordingGB;
        }
        set => Change(ref _maxRecordingGB, Math.Round(Math.Clamp(value, CaptureSettings.MinBytes / 1e9, CaptureSettings.MaxBytes / 1e9), 1));
    }

    /// <summary>"Record calls": a doorbell call is recorded from the moment it opens.</summary>
    public bool RecordCalls
    {
        get
        {
            Load();
            return _recordCalls;
        }
        set => Change(ref _recordCalls, value);
    }

    /// <summary>The consent note was shown.</summary>
    public bool RecordCallsConsentShown
    {
        get
        {
            Load();
            return _recordCallsConsent;
        }
        set => Change(ref _recordCallsConsent, value);
    }

    // Sets a field, saves, and raises Changed, when the value is new.
    private void Change<T>(ref T field, T value)
    {
        Load();
        if (EqualityComparer<T>.Default.Equals(field, value))
        {
            return;
        }
        field = value;
        Save();
        Changed?.Invoke();
    }

    /// <summary>Raised on the UI thread after a setting changes.</summary>
    public event Action? Changed;

    public ChromeCorner IndicatorCorner
    {
        get
        {
            Load();
            return _corner;
        }
        set
        {
            Load();
            if (_corner == value)
            {
                return;
            }
            _corner = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>The corner the user chose for call cards (Settings), null for opposite the indicators.</summary>
    public ChromeCorner? InsetCornerChoice
    {
        get
        {
            Load();
            return _insetCorner;
        }
        set
        {
            Load();
            if (_insetCorner == value)
            {
                return;
            }
            _insetCorner = value;
            Save();
            Changed?.Invoke();
        }
    }

    /// <summary>Where call cards stack from: the chosen corner, else opposite the indicators (the macOS rule).</summary>
    public ChromeCorner InsetCorner => InsetGeometry.Corner(InsetCornerChoice, IndicatorCorner);

    private void Load()
    {
        if (_loaded)
        {
            return;
        }
        _loaded = true;
        if (file is null || !File.Exists(file))
        {
            return;
        }
        try
        {
            using FileStream s = File.OpenRead(file);
            ViewSettingsFile? read = JsonSerializer.Deserialize(s, ViewSettingsJson.Default.ViewSettingsFile);
            _corner = ChromeCorners.Parse(read?.IndicatorCorner);
            _hideOffline = read?.HideOfflineChannels ?? true;
            _insetCorner = read?.InsetCorner is { } ic ? ChromeCorners.Parse(ic) : null;
            _mounts = read?.FisheyeMounts is { } m ? new Dictionary<string, string>(m) : [];
            _talkAllowed = read?.TalkAllowed is { } t ? [.. t] : [];
            _unmuteOnRing = read?.UnmuteOnRing is { } u ? [.. u] : [];
            _entrance = read?.EntranceCameras is { } en ? [.. en] : [];
            _insetGrid = new InsetGridSetting(read?.InsetRows ?? InsetGridSetting.Standard.Rows, read?.InsetColumns ?? InsetGridSetting.Standard.Columns);
            _motionGrants = read?.MotionGrants is { } mg ? mg.Where(kv => kv.Value.Length > 0).ToDictionary(kv => kv.Key, kv => new HashSet<string>(kv.Value)) : [];
            _ptzSpeed = read?.PtzSpeed is { } ps ? Math.Clamp(ps, 1, 64) : 0;
            _saveTranscripts = read?.SaveCallTranscripts ?? false;
            _picturesFolder = string.IsNullOrWhiteSpace(read?.PicturesFolder) ? null : read.PicturesFolder;
            _pictureFormat = PictureFormats.FromKey(read?.PictureFormat);
            _shareSafeNames = read?.ShareSafePictureNames ?? false;
            _exportQuality = ExportPanel.QualityOf(read?.ExportQuality);
            _callsInWindow = read?.CallsOpenInWindow ?? false;
            _callPresentations = read?.CallPresentation is { } cp ? new Dictionary<string, string>(cp) : [];
            _placement = read?.CallWindowPlacement == "activeScreen" ? CallWindowPlacement.Mode.ActiveScreen : CallWindowPlacement.Mode.Remembered;
            _callFrame = read?.CallWindowFrame is { Length: 4 } f && f.All(double.IsFinite) && f[2] > 0 && f[3] > 0 ? new WindowRect(f[0], f[1], f[2], f[3]) : null;
            _settingsFrame = read?.SettingsWindowFrame is { Length: 4 } sf && sf.All(double.IsFinite) && sf[2] > 0 && sf[3] > 0 ? new WindowRect(sf[0], sf[1], sf[2], sf[3]) : null;
            _callOnTop = read?.CallWindowOnTop ?? true;
            _recordingsFolder = read?.RecordingsFolder;
            _recordFrom = RecordQualityNames.Parse(read?.RecordFrom) ?? RecordQuality.Main;
            _maxRecordingMinutes = Math.Clamp(read?.MaxRecordingMinutes ?? 60, CaptureSettings.MinMinutes, CaptureSettings.MaxMinutes);
            _maxRecordingGB = Math.Clamp(read?.MaxRecordingGB ?? 4, CaptureSettings.MinBytes / 1e9, CaptureSettings.MaxBytes / 1e9);
            _recordCalls = read?.RecordCalls ?? false;
            _recordCallsConsent = read?.RecordCallsConsentShown ?? false;
        }
        catch (Exception e) when (e is JsonException or IOException)
        {
            AppLog.Warn($"view-settings.json is unreadable ({e.GetType().Name}); using the defaults");
        }
    }

    private void Save()
    {
        if (file is null)
        {
            return;
        }
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(file)!);
            string temp = file + ".tmp";
            using (FileStream s = File.Create(temp))
            {
                JsonSerializer.Serialize(s, new ViewSettingsFile
                {
                    IndicatorCorner = _corner.Name(),
                    HideOfflineChannels = _hideOffline,
                    InsetCorner = _insetCorner?.Name(),
                    FisheyeMounts = _mounts.Count > 0 ? new Dictionary<string, string>(_mounts) : null,
                    TalkAllowed = _talkAllowed.Count > 0 ? [.. _talkAllowed.Order(StringComparer.Ordinal)] : null,
                    MotionGrants = _motionGrants.Count > 0 ? _motionGrants.ToDictionary(kv => kv.Key, kv => kv.Value.Order(StringComparer.Ordinal).ToArray()) : null,
                    PtzSpeed = _ptzSpeed > 0 && _ptzSpeed != 20 ? _ptzSpeed : null,
                    UnmuteOnRing = _unmuteOnRing.Count > 0 ? [.. _unmuteOnRing.Order(StringComparer.Ordinal)] : null,
                    EntranceCameras = _entrance.Count > 0 ? [.. _entrance.Order(StringComparer.Ordinal)] : null,
                    InsetRows = _insetGrid.Rows == InsetGridSetting.Standard.Rows ? null : _insetGrid.Rows,
                    InsetColumns = _insetGrid.Columns == InsetGridSetting.Standard.Columns ? null : _insetGrid.Columns,
                    SaveCallTranscripts = _saveTranscripts,
                    PicturesFolder = _picturesFolder,
                    PictureFormat = _pictureFormat == PictureFormat.Png ? null : _pictureFormat.Key(),
                    ShareSafePictureNames = _shareSafeNames,
                    ExportQuality = _exportQuality == ExportQuality.Sub ? "sub" : null,
                    CallsOpenInWindow = _callsInWindow,
                    CallPresentation = _callPresentations.Count > 0 ? new Dictionary<string, string>(_callPresentations) : null,
                    CallWindowPlacement = _placement == CallWindowPlacement.Mode.ActiveScreen ? "activeScreen" : null,
                    CallWindowFrame = _callFrame is { } fr ? [fr.X, fr.Y, fr.Width, fr.Height] : null,
                    SettingsWindowFrame = _settingsFrame is { } sw ? [sw.X, sw.Y, sw.Width, sw.Height] : null,
                    CallWindowOnTop = _callOnTop ? null : false,
                    RecordingsFolder = _recordingsFolder,
                    RecordFrom = _recordFrom == RecordQuality.Main ? null : _recordFrom.Name(),
                    MaxRecordingMinutes = _maxRecordingMinutes == 60 ? null : _maxRecordingMinutes,
                    MaxRecordingGB = _maxRecordingGB == 4 ? null : _maxRecordingGB,
                    RecordCalls = _recordCalls,
                    RecordCallsConsentShown = _recordCallsConsent,
                }, ViewSettingsJson.Default.ViewSettingsFile);
                s.Flush(flushToDisk: true);
            }
            File.Move(temp, file, overwrite: true);
        }
        catch (IOException e)
        {
            AppLog.Warn($"saving view-settings.json failed: {e.Message}");
        }
    }
}
