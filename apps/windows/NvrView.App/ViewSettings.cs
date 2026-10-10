// View preferences kept between launches in view-settings.json in the app's
// data directory (separate from the device list in settings.json): today the
// corner that holds the tiles' indicators, and whether the sidebar hides
// offline channels, and the mount of each fisheye camera (docs/UX.md
// "Fisheye cameras"; only those not on the default ceiling mount). Written
// whole and atomically;
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

    /// <summary>Fisheye mounts other than ceiling, by camera ("deviceKey:channel"): "wall" or "desk".</summary>
    public Dictionary<string, string>? FisheyeMounts { get; set; }
}

[JsonSerializable(typeof(ViewSettingsFile))]
[JsonSourceGenerationOptions(WriteIndented = true, PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
internal sealed partial class ViewSettingsJson : JsonSerializerContext;

internal sealed class ViewSettings(string? file)
{
    private ChromeCorner _corner = ChromeCorners.Default;
    private bool _hideOffline = true;
    private bool _loaded;
    private Dictionary<string, string> _mounts = [];

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
            _mounts = read?.FisheyeMounts is { } m ? new Dictionary<string, string>(m) : [];
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
                    FisheyeMounts = _mounts.Count > 0 ? new Dictionary<string, string>(_mounts) : null,
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
