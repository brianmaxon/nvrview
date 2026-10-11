// View preferences kept between launches in view-settings.json in the app's
// data directory (separate from the device list in settings.json): today the
// corner that holds the tiles' indicators, the corner call cards stack from,
// and whether the sidebar hides
// offline channels, and the mount of each fisheye camera (docs/UX.md
// "Fisheye cameras"; only those not on the default ceiling mount). Written
// whole and atomically; the cameras the user allowed talk-back to (docs/UX.md "Doorbell calls");
// unreadable or missing reads as the defaults. Memory only when file is null.

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
            _saveTranscripts = read?.SaveCallTranscripts ?? false;
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
                    SaveCallTranscripts = _saveTranscripts,
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
