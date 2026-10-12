// View preferences kept between launches in view-settings.json in the app's
// data directory (separate from the device list in settings.json): today the
// corner that holds the tiles' indicators, the corner call cards stack from,
// and whether the sidebar hides
// offline channels, and the mount of each fisheye camera (docs/UX.md
// "Fisheye cameras"; only those not on the default ceiling mount). Written
// whole and atomically; the cameras the user allowed talk-back to (docs/UX.md "Doorbell calls");
// the call window's settings (docs/UX.md "A call in its own window"): whether calls open in their
// own window, each doorbell's own choice, where call windows open, the frame the user left one at,
// and its pin; unreadable or missing reads as the defaults. Memory only when file is null.

using System.Text.Json;
using System.Text.Json.Serialization;
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

    /// <summary>Fisheye mounts other than ceiling, by camera ("deviceKey:channel"): "wall" or "desk".</summary>
    public Dictionary<string, string>? FisheyeMounts { get; set; }

    /// <summary>Cameras the user allowed talk-back to ("deviceKey:channel"); camera motion control, off unless listed.</summary>
    public List<string>? TalkAllowed { get; set; }

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

    /// <summary>The call window's pin (always on top); absent: on.</summary>
    public bool? CallWindowOnTop { get; set; }

    /// <summary>Call transcripts go into the log and, one text file per call, the Transcripts folder (off by default).</summary>
    public bool SaveCallTranscripts { get; set; }
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
    private bool _loaded;
    private Dictionary<string, string> _mounts = [];
    private HashSet<string> _talkAllowed = [];
    private HashSet<string> _unmuteOnRing = [];
    private bool _callsInWindow;
    private Dictionary<string, string> _callPresentations = [];
    private CallWindowPlacement.Mode _placement = CallWindowPlacement.Mode.Remembered;
    private WindowRect? _callFrame;
    private bool _callOnTop = true;

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
            _saveTranscripts = read?.SaveCallTranscripts ?? false;
            _callsInWindow = read?.CallsOpenInWindow ?? false;
            _callPresentations = read?.CallPresentation is { } cp ? new Dictionary<string, string>(cp) : [];
            _placement = read?.CallWindowPlacement == "activeScreen" ? CallWindowPlacement.Mode.ActiveScreen : CallWindowPlacement.Mode.Remembered;
            _callFrame = read?.CallWindowFrame is { Length: 4 } f && f.All(double.IsFinite) && f[2] > 0 && f[3] > 0 ? new WindowRect(f[0], f[1], f[2], f[3]) : null;
            _callOnTop = read?.CallWindowOnTop ?? true;
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
                    UnmuteOnRing = _unmuteOnRing.Count > 0 ? [.. _unmuteOnRing.Order(StringComparer.Ordinal)] : null,
                    SaveCallTranscripts = _saveTranscripts,
                    CallsOpenInWindow = _callsInWindow,
                    CallPresentation = _callPresentations.Count > 0 ? new Dictionary<string, string>(_callPresentations) : null,
                    CallWindowPlacement = _placement == CallWindowPlacement.Mode.ActiveScreen ? "activeScreen" : null,
                    CallWindowFrame = _callFrame is { } fr ? [fr.X, fr.Y, fr.Width, fr.Height] : null,
                    CallWindowOnTop = _callOnTop ? null : false,
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
