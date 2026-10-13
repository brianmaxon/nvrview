// The settings document the core is configured with (docs/FFI.md, "Smart
// alerts"), built from the user's stored alerts.json. The same rules as the
// macOS AlertSettingsDocument: cameras named by device_key become this run's
// device IDs, utc_offset_min is added, and secrets are filled from the
// credential store, never from the file.

using System.Text.Json;
using System.Text.Json.Nodes;

namespace NvrView.Shell;

/// <summary>
/// Where credentials come from (Credential Manager in the app, memory in
/// tests). Read at startup and passed to the core in memory.
/// </summary>
public interface IAlertSecrets
{
    /// <summary>The credential kept for an input's or output's id, if there is one.</summary>
    string? Secret(string id);
}

/// <summary>A fixed set of credentials, for tests and the self-test.</summary>
public sealed class MemoryAlertSecrets(IReadOnlyDictionary<string, string>? values = null) : IAlertSecrets
{
    private readonly IReadOnlyDictionary<string, string> _values = values ?? new Dictionary<string, string>();

    public string? Secret(string id) => _values.GetValueOrDefault(id);
}

/// <summary>The built document.</summary>
/// <param name="Json">The JSON for AlertsConfigure.</param>
/// <param name="Enabled">The stored document's <c>enabled</c> (off unless it says true).</param>
/// <param name="Skipped">Entries left out because their device is not added.</param>
public sealed record BuiltAlertSettings(string Json, bool Enabled, int Skipped);

public static class AlertSettingsDocument
{
    /// <summary>
    /// Builds the document. <paramref name="deviceId"/> maps a device's stable
    /// key to this run's device ID (null: not added; the nearest array element
    /// naming it is left out). A <c>secrets</c> member of the stored file is
    /// ignored: credentials are never kept there. Null when the file is not a
    /// settings object.
    /// </summary>
    public static BuiltAlertSettings? Build(
        string stored, Func<string, ulong?> deviceId, int utcOffsetMinutes, IAlertSecrets secrets)
    {
        JsonObject? root;
        try
        {
            root = JsonNode.Parse(stored) as JsonObject;
        }
        catch (JsonException)
        {
            return null;
        }
        if (root is null)
        {
            return null;
        }
        int skipped = 0;
        JsonObject doc = [];
        foreach ((string key, JsonNode? value) in root)
        {
            if (key == "secrets")
            {
                continue;
            }
            if (Resolve(value, deviceId, ref skipped, out JsonNode? resolved))
            {
                doc[key] = resolved;
            }
        }
        bool enabled = root["enabled"] is JsonValue v && v.TryGetValue(out bool b) && b;
        doc["enabled"] = enabled;
        doc["utc_offset_min"] = utcOffsetMinutes;
        JsonObject found = [];
        foreach (JsonNode? entry in (doc["inputs"] as JsonArray) ?? [])
        {
            if (entry is JsonObject o && Str(o, "kind") == "mqtt" && Str(o, "id") is { } id && secrets.Secret(id) is { } s)
            {
                found[id] = s;
            }
        }
        foreach (JsonNode? entry in (doc["outputs"] as JsonArray) ?? [])
        {
            if (entry is JsonObject o && Str(o, "kind") == "home_assistant")
            {
                string id = Str(o, "id") ?? "home_assistant";
                if (secrets.Secret(id) is { } s)
                {
                    found[id] = s;
                }
            }
        }
        if (found.Count > 0)
        {
            doc["secrets"] = found;
        }
        return new BuiltAlertSettings(doc.ToJsonString(), enabled, skipped);
    }

    private static string? Str(JsonObject o, string name) =>
        o[name] is JsonValue v && v.TryGetValue(out string? s) ? s : null;

    /// <summary>Replaces device_key with device; false when an object names a device that is not added.</summary>
    private static bool Resolve(JsonNode? node, Func<string, ulong?> deviceId, ref int skipped, out JsonNode? result)
    {
        switch (node)
        {
            case JsonObject obj:
                JsonObject copy = [];
                foreach ((string key, JsonNode? inner) in obj)
                {
                    if (key == "device_key")
                    {
                        if (inner is JsonValue kv && kv.TryGetValue(out string? k) && deviceId(k) is { } id)
                        {
                            copy["device"] = id;
                            continue;
                        }
                        result = null;
                        return false;
                    }
                    if (!Resolve(inner, deviceId, ref skipped, out JsonNode? r))
                    {
                        result = null;
                        return false;
                    }
                    copy[key] = r;
                }
                result = copy;
                return true;
            case JsonArray array:
                JsonArray list = [];
                foreach (JsonNode? element in array)
                {
                    if (Resolve(element, deviceId, ref skipped, out JsonNode? r))
                    {
                        list.Add(r);
                    }
                    else
                    {
                        skipped++;
                    }
                }
                result = list;
                return true;
            default:
                result = node?.DeepClone();
                return true;
        }
    }
}
