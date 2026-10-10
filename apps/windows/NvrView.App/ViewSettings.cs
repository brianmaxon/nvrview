// View preferences kept between launches in view-settings.json in the app's
// data directory (separate from the device list in settings.json): today the
// corner that holds the tiles' indicators. Written whole and atomically;
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
}

[JsonSerializable(typeof(ViewSettingsFile))]
[JsonSourceGenerationOptions(WriteIndented = true, PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
internal sealed partial class ViewSettingsJson : JsonSerializerContext;

internal sealed class ViewSettings(string? file)
{
    private ChromeCorner _corner = ChromeCorners.Default;
    private bool _loaded;

    /// <summary>Raised on the UI thread after the corner changes.</summary>
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
            _corner = ChromeCorners.Parse(JsonSerializer.Deserialize(s, ViewSettingsJson.Default.ViewSettingsFile)?.IndicatorCorner);
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
                JsonSerializer.Serialize(s, new ViewSettingsFile { IndicatorCorner = _corner.Name() }, ViewSettingsJson.Default.ViewSettingsFile);
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
