// The credentials smart alerts use (an MQTT password, a Home Assistant access
// token) in Credential Manager, named "nvrview/alerts/<id>" after the id of
// the input or output in alerts.json. Each is read once per launch, kept in
// memory, and passed to the core with the settings (docs/FFI.md, "Smart
// alerts"); never written to a settings file or a log.

using NvrView.Shell;

namespace NvrView.App;

internal sealed class WindowsAlertSecrets : IAlertSecrets
{
    private const string Prefix = "nvrview/alerts/";
    private readonly WindowsCredentials _store = new(Prefix);
    private readonly Dictionary<string, string?> _cache = [];

    public string? Secret(string id)
    {
        lock (_cache)
        {
            if (!_cache.TryGetValue(id, out string? value))
            {
                value = _store.Read(id);
                _cache[id] = value;
            }
            return value;
        }
    }

    /// <summary>Stores or replaces a credential (for the settings screen that will ask for it).</summary>
    public bool Save(string id, string value)
    {
        bool ok = _store.Write(id, "nvrview", value);
        if (ok)
        {
            lock (_cache)
            {
                _cache[id] = value;
            }
        }
        return ok;
    }
}
