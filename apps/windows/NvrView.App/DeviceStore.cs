// The configured devices, kept between launches in settings.json in the
// app's data directory, written whole and atomically. No secret in here:
// each device's password is in Credential Manager under its Key.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace NvrView.App;

/// <summary>A configured device.</summary>
internal sealed class DeviceConfig
{
    /// <summary>The app's own stable key for the device; names its credential.</summary>
    public string Key { get; set; } = NewKey();

    public string Name { get; set; } = "";

    public string Host { get; set; } = "";

    public ushort Port { get; set; } = 9000;

    public string User { get; set; } = "";

    /// <summary>The core's device key (nv_device_key) once known, for layouts (W3).</summary>
    public string? CoreKey { get; set; }

    public static string NewKey() => "dev-" + Guid.NewGuid().ToString("N")[..12];

    public DeviceConfig Copy() => (DeviceConfig)MemberwiseClone();
}

internal sealed class SettingsFile
{
    public int Version { get; set; } = 1;

    public List<DeviceConfig> Devices { get; set; } = [];
}

[JsonSerializable(typeof(SettingsFile))]
[JsonSourceGenerationOptions(WriteIndented = true, PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
internal sealed partial class SettingsJson : JsonSerializerContext;

/// <summary>settings.json, or memory only when <c>file</c> is null.</summary>
internal sealed class DeviceStore(string? file)
{
    public List<DeviceConfig> Load()
    {
        if (file is null || !File.Exists(file))
        {
            return [];
        }
        try
        {
            using FileStream s = File.OpenRead(file);
            return JsonSerializer.Deserialize(s, SettingsJson.Default.SettingsFile)?.Devices ?? [];
        }
        catch (Exception e) when (e is JsonException or IOException)
        {
            AppLog.Warn($"settings.json is unreadable ({e.GetType().Name}); keeping it as settings.json.unreadable");
            try
            {
                File.Move(file, file + ".unreadable", overwrite: true);
            }
            catch (IOException)
            {
            }
            return [];
        }
    }

    public void Save(IEnumerable<DeviceConfig> devices)
    {
        if (file is null)
        {
            return;
        }
        Directory.CreateDirectory(Path.GetDirectoryName(file)!);
        string temp = file + ".tmp";
        using (FileStream s = File.Create(temp))
        {
            JsonSerializer.Serialize(s, new SettingsFile { Devices = [.. devices] }, SettingsJson.Default.SettingsFile);
            s.Flush(flushToDisk: true);
        }
        File.Move(temp, file, overwrite: true);
    }
}
