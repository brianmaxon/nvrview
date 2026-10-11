// Self-update as the Windows app does it (docs/UPDATES.md), the parts that
// need no window: the core's update events, the settings and their
// schedule, the request JSON, and the side-by-side install (a new
// NvrView-<version> folder next to the running one; the running folder is
// never modified). Plain .NET, tested by NvrView.Shell.Tests.

using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace NvrView.Shell;

/// <summary>One <c>update</c> event (docs/FFI.md, "Updates").</summary>
public sealed record UpdateEvent(
    ulong Op, string Action, string State, string? Current = null, string? Version = null, string? Latest = null,
    string? Notes = null, ulong? Received = null, ulong? Total = null, string? Path = null, string? Reason = null,
    string? Message = null, bool NoPackage = false)
{
    /// <summary>A final event: nothing more of its operation follows.</summary>
    public bool IsFinal => State is not ("started" or "downloading");

    public static UpdateEvent? Parse(ReadOnlySpan<byte> json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json.ToArray());
            JsonElement r = doc.RootElement;
            if (r.ValueKind != JsonValueKind.Object || Str(r, "type") != "update" ||
                !r.TryGetProperty("op", out JsonElement op) || !op.TryGetUInt64(out ulong id) ||
                Str(r, "action") is not { } action || Str(r, "state") is not { } state)
            {
                return null;
            }
            return new UpdateEvent(
                id, action, state, Str(r, "current"), Str(r, "version"), Str(r, "latest"), Str(r, "notes"),
                U64(r, "received"), U64(r, "total"), Str(r, "path"), Str(r, "reason"), Str(r, "message"),
                r.TryGetProperty("no_package", out JsonElement np) && np.ValueKind == JsonValueKind.True);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static string? Str(JsonElement r, string name) =>
        r.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static ulong? U64(JsonElement r, string name) =>
        r.TryGetProperty(name, out JsonElement v) && v.TryGetUInt64(out ulong n) ? n : null;
}

/// <summary>What the user set in Settings, Updates; kept in update-settings.json with the other settings.</summary>
public sealed class UpdateSettings
{
    /// <summary>A folder or URL; empty: updates are off.</summary>
    public string Source { get; set; } = "";

    /// <summary>Canonical public key lines (<c>nvrview-ed25519-pub &lt;hex&gt;</c>).</summary>
    public List<string> Keys { get; set; } = [];

    /// <summary>Check at startup and about every 24 hours.</summary>
    public bool Automatic { get; set; }

    /// <summary>When the last check finished, wall-clock time.</summary>
    public DateTimeOffset? LastCheck { get; set; }

    /// <summary>A source and at least one key: a check can run.</summary>
    public bool CanCheck => !string.IsNullOrWhiteSpace(Source) && Keys.Count > 0;

    public static UpdateSettings Load(string? file)
    {
        var s = new UpdateSettings();
        if (file is null || !File.Exists(file))
        {
            return s;
        }
        try
        {
            if (JsonNode.Parse(File.ReadAllText(file)) is not JsonObject o)
            {
                return s;
            }
            s.Source = o["source"]?.GetValue<string>() ?? "";
            if (o["keys"] is JsonArray keys)
            {
                s.Keys = [.. keys.Select(k => k?.GetValue<string>()).OfType<string>()];
            }
            s.Automatic = o["automatic"]?.GetValue<bool>() ?? false;
            if (o["last_check"]?.GetValue<string>() is { } last &&
                DateTimeOffset.TryParse(last, System.Globalization.CultureInfo.InvariantCulture, System.Globalization.DateTimeStyles.RoundtripKind, out DateTimeOffset at))
            {
                s.LastCheck = at;
            }
            return s;
        }
        catch (Exception e) when (e is JsonException or IOException or UnauthorizedAccessException or InvalidOperationException or FormatException)
        {
            return new UpdateSettings();
        }
    }

    public void Save(string? file)
    {
        if (file is null)
        {
            return;
        }
        var o = new JsonObject
        {
            ["source"] = Source,
            ["keys"] = new JsonArray([.. Keys.Select(k => (JsonNode?)JsonValue.Create(k))]),
            ["automatic"] = Automatic,
        };
        if (LastCheck is { } at)
        {
            o["last_check"] = at.ToString("O", System.Globalization.CultureInfo.InvariantCulture);
        }
        Directory.CreateDirectory(System.IO.Path.GetDirectoryName(file)!);
        string tmp = file + ".tmp";
        File.WriteAllText(tmp, o.ToJsonString());
        File.Move(tmp, file, overwrite: true);
    }
}

/// <summary>When automatic checks run: a minute after launch, then about every 24 hours from the last check (kept across launches).</summary>
public sealed class UpdateSchedule
{
    public TimeSpan Interval { get; init; } = TimeSpan.FromHours(24);

    public TimeSpan StartupDelay { get; init; } = TimeSpan.FromMinutes(1);

    public bool IsDue(UpdateSettings s, DateTimeOffset now, DateTimeOffset launchedAt)
    {
        if (!s.Automatic || !s.CanCheck || now - launchedAt < StartupDelay)
        {
            return false;
        }
        if (s.LastCheck is not { } last)
        {
            return true;
        }
        TimeSpan since = now - last;
        // A clock set back makes the last check look in the future: due.
        return since >= Interval || since < TimeSpan.Zero;
    }
}

/// <summary>The updater's state, for the status line and the menu.</summary>
public sealed record UpdateStatus(UpdateStatus.Kind State, string Version = "", string Notes = "", double Fraction = 0, string Path = "", string Message = "", DateTimeOffset At = default)
{
    public enum Kind
    {
        Idle,
        Checking,
        UpToDate,
        Available,
        Downloading,
        Ready,
        Failed,
    }

    public static readonly UpdateStatus Idle = new(Kind.Idle);

    public string Line(string current) => State switch
    {
        Kind.Checking => "Checking for updates…",
        Kind.UpToDate => $"Up to date ({current}), checked {At.ToLocalTime():g}.",
        Kind.Available => $"Version {Version} is available.",
        Kind.Downloading => $"Downloading {Version}… {Math.Round(Fraction * 100)}%",
        Kind.Ready => $"Version {Version} is installed next to this one and ready to start.",
        Kind.Failed => Message,
        _ => $"Version {current}.",
    };

    /// <summary>Reduces an event of the current operation; null: no change.</summary>
    public static UpdateStatus? After(UpdateEvent e, DateTimeOffset now, string? pending) => e.State switch
    {
        "started" => e.Action == "check" ? new(Kind.Checking) : new(Kind.Downloading, pending ?? ""),
        "up_to_date" => new(Kind.UpToDate, e.Latest ?? "", At: now),
        "available" => new(Kind.Available, e.Version ?? "", e.Notes ?? ""),
        "downloading" => new(Kind.Downloading, pending ?? "", Fraction: e.Total is > 0 ? Math.Clamp((double)(e.Received ?? 0) / e.Total.Value, 0, 1) : 0),
        "ready" when e.Version is { } v && e.Path is { } p => new(Kind.Ready, v, Path: p),
        "ready" => new(Kind.Failed, Message: "The update could not be prepared."),
        "failed" => new(Kind.Failed, Message: Describe(e.Reason, e.Message)),
        "canceled" => Idle,
        _ => null,
    };

    public static string Describe(string? reason, string? detail)
    {
        string lead = reason switch
        {
            "no_trusted_keys" => "Add the update publisher's public key first.",
            "bad_signature" => "The update's signature did not verify with any trusted key.",
            "insecure_source" => "Plain http:// is allowed only on the local network.",
            "source_invalid" => "The update source is not a folder or URL NvrView can use.",
            "unreachable" => "The update source could not be reached.",
            "not_found" => "No update was found at the source.",
            "bad_package" or "unsafe_archive" => "The update package was refused.",
            "staging" => "The update could not be prepared next to the app.",
            "unsupported_cpu" => "This computer cannot use HTTPS update sources; use a folder.",
            _ => "The update check failed.",
        };
        return string.IsNullOrEmpty(detail) ? lead : $"{lead} ({detail})";
    }
}

/// <summary>The JSON for nv_update_check and nv_update_download.</summary>
public static class UpdateRequest
{
    public static string Json(UpdateSettings s, string current, string? version = null, string? staging = null)
    {
        var o = new JsonObject
        {
            ["source"] = s.Source.Trim(),
            ["keys"] = new JsonArray([.. s.Keys.Select(k => (JsonNode?)JsonValue.Create(k))]),
            ["current"] = current,
            ["platform"] = "win-x64",
        };
        if (version is not null && staging is not null)
        {
            o["version"] = version;
            o["staging_dir"] = staging;
        }
        return o.ToJsonString();
    }
}

/// <summary>
/// Side-by-side versions (docs/UPDATES.md, "Applying", Windows): staging in
/// <c>.NvrView-update</c> next to the app folder, the unpacked update moved
/// to a new <c>NvrView-&lt;version&gt;</c> folder beside it. The running
/// folder and the data directory are never touched.
/// </summary>
public static class SideBySide
{
    public const string FolderName = ".NvrView-update";

    /// <summary>The marker written into an installed version folder (its version).</summary>
    public const string Marker = ".nvrview-version";

    public const string Executable = "NvrView.App.exe";

    /// <summary>The staging folder's path for the app in <paramref name="appDir"/>, without creating it.</summary>
    public static string FolderPath(string appDir) =>
        System.IO.Path.Combine(System.IO.Path.GetDirectoryName(System.IO.Path.TrimEndingDirectorySeparator(System.IO.Path.GetFullPath(appDir)))!, FolderName);

    /// <summary>Where updates of the app in <paramref name="appDir"/> stage (created), or why they cannot.</summary>
    public static string? UpdateFolder(string appDir, out string? problem)
    {
        string folder = FolderPath(appDir);
        string parent = System.IO.Path.GetDirectoryName(folder)!;
        try
        {
            Directory.CreateDirectory(folder);
            string probe = System.IO.Path.Combine(folder, ".write-test");
            File.WriteAllText(probe, "");
            File.Delete(probe);
            problem = null;
            return folder;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            problem = $"NvrView cannot write next to its folder ({parent}). Move the NvrView folder somewhere you own, such as %LOCALAPPDATA%\\Programs, and update again.";
            return null;
        }
    }

    public static string NewStagingPath(string folder, string version) =>
        System.IO.Path.Combine(folder, $"staging-{version}-{Guid.NewGuid().ToString("N")[..8]}");

    /// <summary>Removes staging leftovers of interrupted downloads.</summary>
    public static void RemoveLeftovers(string folder)
    {
        if (!Directory.Exists(folder))
        {
            return;
        }
        foreach (string d in Directory.EnumerateDirectories(folder, "staging-*"))
        {
            try
            {
                Directory.Delete(d, recursive: true);
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                // Still in use: next time.
            }
        }
    }

    /// <summary>
    /// Moves the unpacked update (<paramref name="content"/>, the core's
    /// <c>ready</c> path) to <c>NvrView-&lt;version&gt;</c> beside
    /// <paramref name="appDir"/> (<c>-2</c>, <c>-3</c>, ... when taken) and
    /// marks it. Refuses content without the app's executable. Returns the
    /// new folder.
    /// </summary>
    public static string Install(string content, string appDir, string version)
    {
        if (!File.Exists(System.IO.Path.Combine(content, Executable)))
        {
            throw new InvalidDataException($"the update has no {Executable}");
        }
        string parent = System.IO.Path.GetDirectoryName(System.IO.Path.TrimEndingDirectorySeparator(System.IO.Path.GetFullPath(appDir)))!;
        string target = System.IO.Path.Combine(parent, $"NvrView-{version}");
        for (int n = 2; Directory.Exists(target) || File.Exists(target); n++)
        {
            target = System.IO.Path.Combine(parent, $"NvrView-{version}-{n}");
        }
        Directory.Move(content, target);
        File.WriteAllText(System.IO.Path.Combine(target, Marker), version);
        return target;
    }

    /// <summary>A folder beside <paramref name="appDir"/> that an earlier update installed for <paramref name="version"/>, if any.</summary>
    public static string? FindInstalled(string appDir, string version)
    {
        string parent = System.IO.Path.GetDirectoryName(System.IO.Path.TrimEndingDirectorySeparator(System.IO.Path.GetFullPath(appDir)))!;
        foreach (string d in Directory.EnumerateDirectories(parent, $"NvrView-{version}*").Order(StringComparer.Ordinal))
        {
            string marker = System.IO.Path.Combine(d, Marker);
            if (File.Exists(marker) && File.ReadAllText(marker).Trim() == version && File.Exists(System.IO.Path.Combine(d, Executable)))
            {
                return d;
            }
        }
        return null;
    }

    /// <summary>Waits for process <paramref name="pid"/> to exit; true when it has (or never existed).</summary>
    public static bool WaitForExit(int pid, TimeSpan limit)
    {
        Process p;
        try
        {
            p = Process.GetProcessById(pid);
        }
        catch (ArgumentException)
        {
            return true;
        }
        using (p)
        {
            try
            {
                return p.WaitForExit(limit);
            }
            catch (InvalidOperationException)
            {
                return true;
            }
            catch (System.ComponentModel.Win32Exception)
            {
                // Not allowed to wait on it (another session): poll for it instead.
            }
        }
        var deadline = Stopwatch.StartNew();
        while (deadline.Elapsed < limit)
        {
            try
            {
                using Process still = Process.GetProcessById(pid);
                if (still.HasExited)
                {
                    return true;
                }
            }
            catch (ArgumentException)
            {
                return true;
            }
            catch (Exception e) when (e is InvalidOperationException or System.ComponentModel.Win32Exception)
            {
                // HasExited may be refused too; existence is what counts.
            }
            Thread.Sleep(200);
        }
        return false;
    }
}
