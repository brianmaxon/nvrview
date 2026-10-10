// Device discovery as the app sees it (docs/FFI.md, "Device discovery",
// minor 1 of ABI 3): the events a search sends, and DiscoveryList, the state
// the add-device screen's "Find Devices on This Network" list shows. Plain
// .NET, tested on Linux. The core never sends device IDs or MAC addresses,
// so none are here.

using System.Text.Json;

namespace NvrView.Shell;

/// <summary>How a found device can be reached.</summary>
public sealed record DiscoveredService(string Kind, int? Port = null, string? Url = null);

/// <summary>A device that answered a search.</summary>
public sealed record DiscoveredDevice(
    ulong Search,
    string Key,
    string Kind,
    string? Name,
    string? Model,
    string Address,
    int? Port,
    int? Channels,
    int? BatteryPercent,
    IReadOnlyList<DiscoveredService> Services)
{
    /// <summary>The viewer speaks only the native protocol so far; a device that offered nothing else (an ONVIF camera) cannot be added.</summary>
    public bool IsNative => Services.Count == 0 || Services.Any(s => s.Kind == "native");

    /// <summary>The port to fill into the form: the answer's, else the usual 9000.</summary>
    public int SuggestedPort => Port ?? 9000;

    /// <summary>What to call it in a list.</summary>
    public string Title => Name is { Length: > 0 } n ? n : Kind switch
    {
        "nvr" => "NVR",
        "camera" => "Camera",
        _ => "Device",
    };

    /// <summary>Model and address, as the list shows them.</summary>
    public string Detail => string.Join(" · ", new[] { Model, Port is { } p ? $"{Address}:{p}" : Address }.Where(s => !string.IsNullOrEmpty(s)));

    /// <summary>Whether the user already added it: the same address and port.</summary>
    public bool IsAdded(IEnumerable<(string Host, int Port)> added) =>
        added.Any(a => string.Equals(a.Host, Address, StringComparison.OrdinalIgnoreCase) && a.Port == SuggestedPort);
}

/// <summary>How a search ended (<c>discovery_done</c>).</summary>
public sealed record DiscoveryEnd(ulong Search, int Count, bool Stopped, IReadOnlyList<string> Errors);

/// <summary>One parsed discovery event: exactly one of the members is set.</summary>
public sealed record DiscoveryEvent(ulong? Started, DiscoveredDevice? Device, DiscoveryEnd? Done)
{
    /// <summary>Parses a <c>discovery_started</c>, <c>discovered</c>, or <c>discovery_done</c> event; null for anything else or a malformed one.</summary>
    public static DiscoveryEvent? Parse(ReadOnlySpan<byte> json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json.ToArray());
            JsonElement r = doc.RootElement;
            if (r.ValueKind != JsonValueKind.Object || !r.TryGetProperty("type", out JsonElement t) ||
                !r.TryGetProperty("search", out JsonElement s) || !s.TryGetUInt64(out ulong search))
            {
                return null;
            }
            switch (t.GetString())
            {
                case "discovery_started":
                    return new DiscoveryEvent(search, null, null);
                case "discovered":
                    if (Str(r, "key") is not { } key || Str(r, "address") is not { } address)
                    {
                        return null;
                    }
                    var services = new List<DiscoveredService>();
                    if (r.TryGetProperty("services", out JsonElement list) && list.ValueKind == JsonValueKind.Array)
                    {
                        foreach (JsonElement x in list.EnumerateArray())
                        {
                            if (Str(x, "kind") is { } kind)
                            {
                                services.Add(new DiscoveredService(kind, Int(x, "port"), Str(x, "url")));
                            }
                        }
                    }
                    return new DiscoveryEvent(
                        null,
                        new DiscoveredDevice(
                            search, key, Str(r, "kind") ?? "unknown", Str(r, "name"), Str(r, "model"), address,
                            Int(r, "port"), Int(r, "channels"), Int(r, "battery_percent"), services),
                        null);
                case "discovery_done":
                    var errors = new List<string>();
                    if (r.TryGetProperty("errors", out JsonElement errs) && errs.ValueKind == JsonValueKind.Array)
                    {
                        errors.AddRange(errs.EnumerateArray().Select(e => e.GetString() ?? "").Where(e => e.Length > 0));
                    }
                    return new DiscoveryEvent(
                        null, null,
                        new DiscoveryEnd(
                            search, Int(r, "count") ?? 0,
                            r.TryGetProperty("stopped", out JsonElement st) && st.ValueKind == JsonValueKind.True, errors));
                default:
                    return null;
            }
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static string? Str(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static int? Int(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.TryGetInt32(out int i) ? i : null;
}

/// <summary>
/// What the "find devices" list shows: the devices of the current search and
/// whether it is still running. The app calls <see cref="Requested"/> after
/// each successful start and <see cref="Stopped"/> after its own stop, and
/// <see cref="Apply"/> with every discovery event. Starts in quick succession
/// are told apart by <c>discovery_started</c>: only the events of the last
/// requested search count, so a stopped search's tail never lands in the new
/// list.
/// </summary>
public sealed class DiscoveryList
{
    public enum PhaseKind
    {
        /// <summary>No search was requested, or it was stopped.</summary>
        Idle,
        Searching,
        /// <summary>The search ran its time out.</summary>
        Finished,
    }

    private readonly List<DiscoveredDevice> _devices = [];
    private int _pendingStarts;
    private ulong? _current;

    public PhaseKind Phase { get; private set; }

    public IReadOnlyList<DiscoveredDevice> Devices => _devices;

    /// <summary>The search whose events count (known once its <c>discovery_started</c> arrived).</summary>
    public ulong? CurrentSearch => _current;

    /// <summary>The core could not start a search (it predates discovery, or refused).</summary>
    public bool Unavailable { get; private set; }

    /// <summary>How the last search ended, once it did.</summary>
    public DiscoveryEnd? End { get; private set; }

    public bool IsSearching => Phase == PhaseKind.Searching;

    /// <summary>
    /// A search that ended with nothing. The usual cause is a blocked
    /// network permission (macOS Local Network, Windows Firewall): the system
    /// drops the traffic without an error, so the screen points there.
    /// </summary>
    public bool FoundNothing => Phase == PhaseKind.Finished && _devices.Count == 0 && End is { Count: 0 };

    /// <summary>
    /// Nothing found, and the system refused the sends ("No route to host"):
    /// macOS answers that way while Local Network access is denied or still
    /// waiting for the user.
    /// </summary>
    public bool PermissionLikelyBlocked =>
        FoundNothing && Errors.Any(e => e.Contains("no route to host", StringComparison.OrdinalIgnoreCase));

    /// <summary>Problems the last search reported, for a details view.</summary>
    public IReadOnlyList<string> Errors => Phase == PhaseKind.Finished && End is { } e ? e.Errors : [];

    /// <summary>A search was started (<c>nv_discover_start</c> returned OK): the list empties.</summary>
    public void Requested()
    {
        _pendingStarts++;
        _current = null;
        _devices.Clear();
        Unavailable = false;
        End = null;
        Phase = PhaseKind.Searching;
    }

    /// <summary>A start was refused: nothing runs.</summary>
    public void FailedToStart()
    {
        _current = null;
        _devices.Clear();
        End = null;
        Unavailable = true;
        Phase = PhaseKind.Idle;
    }

    /// <summary>The app stopped the search itself.</summary>
    public void Stopped()
    {
        if (Phase == PhaseKind.Searching)
        {
            Phase = PhaseKind.Idle;
        }
    }

    public void Apply(DiscoveryEvent e)
    {
        if (e.Started is { } started)
        {
            if (_pendingStarts > 0 && --_pendingStarts == 0)
            {
                _current = started;
            }
        }
        else if (e.Device is { } device)
        {
            if (Phase != PhaseKind.Searching || device.Search != _current)
            {
                return;
            }
            int i = _devices.FindIndex(d => d.Key == device.Key);
            if (i >= 0)
            {
                _devices[i] = device;
            }
            else
            {
                _devices.Add(device);
            }
        }
        else if (e.Done is { } done && done.Search == _current && Phase == PhaseKind.Searching)
        {
            End = done;
            Phase = PhaseKind.Finished;
        }
    }
}
