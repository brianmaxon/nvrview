// The caption engine's download (docs/ARCHITECTURE.md "Live transcription", apps/windows/README.md
// "Captions"): the Moonshine streaming model with the engine's native libraries, fetched on first
// use from the `models` folder next to the user's update source, checked against the size and
// SHA-256 this build pins (scripts/caption-model.json, embedded in the app), and unpacked into the
// per-user Models folder. The same pattern as the macOS detector model
// (apps/macos/Sources/AlertHost/ModelInstaller.swift). Plain .NET, tested by NvrView.Shell.Tests
// with folder sources; nothing here touches the network in tests.

using System.Net;
using System.Security.Cryptography;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace NvrView.Shell;

/// <summary>
/// What a build pins for the caption model's archive: its name, size and SHA-256, the files it
/// must hold, and the Moonshine architecture of the model in it (MOONSHINE_MODEL_ARCH_*: 2 tiny,
/// 4 small, 5 medium streaming).
/// </summary>
public sealed record CaptionModelManifest(string Name, long Bytes, string Sha256, IReadOnlyList<string> Files, uint Arch = 4)
{
    /// <summary>The embedded resource name of the manifest in the app.</summary>
    public const string ResourceName = "caption-model.json";

    /// <summary>Whether the manifest pins a published archive (an unpublished one is never downloaded).</summary>
    public bool IsPublished => Bytes > 0 && Sha256.Length == 64 && Sha256.All(Uri.IsHexDigit) && Name.Length > 0 && Arch is >= 2 and <= 5;

    /// <summary>The archive's file name in the models folder.</summary>
    public string ArchiveName => Name + ".zip";

    /// <summary>Reads scripts/caption-model.json; null when it is malformed or names no safe model.</summary>
    public static CaptionModelManifest? Parse(string json)
    {
        try
        {
            if (JsonNode.Parse(json) is not JsonObject o ||
                o["name"]?.GetValue<string>() is not { Length: > 0 } name ||
                name.Any(c => !(char.IsAsciiLetterOrDigit(c) || c is '-' or '_' or '.')) || name.StartsWith('.'))
            {
                return null;
            }
            long bytes = o["bytes"] is JsonValue b && b.TryGetValue(out long n) ? n : 0;
            string sha = o["sha256"] is JsonValue s && s.TryGetValue(out string? hex) ? hex.ToLowerInvariant() : "";
            var files = new List<string>();
            if (o["files"] is JsonArray array)
            {
                foreach (JsonNode? node in array)
                {
                    if (node is JsonValue v && v.TryGetValue(out string? f) && IsPlainName(f))
                    {
                        files.Add(f);
                    }
                }
            }
            uint arch = o["model_arch"] is JsonValue a && a.TryGetValue(out uint m) ? m : 0;
            return new CaptionModelManifest(name, bytes, sha, files, arch);
        }
        catch (Exception e) when (e is JsonException or InvalidOperationException or FormatException)
        {
            return null;
        }
    }

    private static bool IsPlainName(string f) =>
        f.Length > 0 && f.IndexOfAny(['/', '\\', ':']) < 0 && f != "." && f != "..";

    /// <summary>
    /// Where the archive is: <c>&lt;source&gt;/models/&lt;name&gt;.zip</c>. The source is the update
    /// source (docs/UPDATES.md): an absolute folder (a drive path or a UNC share), a <c>file://</c>
    /// or <c>https://</c> URL, or an <c>http://</c> URL to a host on the local network; a URL may
    /// name the folder or its <c>updates.json</c>. Null for anything else.
    /// </summary>
    public string? ArchiveLocation(string updateSource)
    {
        string text = updateSource.Trim();
        if (text.Length == 0)
        {
            return null;
        }
        if (Uri.TryCreate(text, UriKind.Absolute, out Uri? uri) && (uri.Scheme is "http" or "https"))
        {
            if (uri.UserInfo.Length > 0 || (uri.Scheme == "http" && !IsLocalHost(uri.Host)))
            {
                return null;
            }
            string path = uri.AbsolutePath;
            if (path.EndsWith("/updates.json", StringComparison.OrdinalIgnoreCase))
            {
                path = path[..^"updates.json".Length];
            }
            if (!path.EndsWith('/'))
            {
                path += "/";
            }
            return new UriBuilder(uri) { Path = path + "models/" + ArchiveName, Query = "", Fragment = "" }.Uri.AbsoluteUri;
        }
        string? folder = null;
        if (uri is { IsFile: true } && text.StartsWith("file:", StringComparison.OrdinalIgnoreCase))
        {
            folder = uri.LocalPath;
        }
        else if (text.StartsWith(@"\\", StringComparison.Ordinal) || text.StartsWith('/') ||
            (text.Length >= 3 && char.IsAsciiLetter(text[0]) && text[1] == ':' && text[2] is '\\' or '/'))
        {
            folder = text;
        }
        if (folder is null)
        {
            return null;
        }
        if (folder.EndsWith("updates.json", StringComparison.OrdinalIgnoreCase))
        {
            folder = folder[..^"updates.json".Length];
        }
        return Path.Combine(folder.TrimEnd('\\', '/'), "models", ArchiveName);
    }

    /// <summary>Loopback, a private or link-local address, <c>localhost</c>, or a <c>.local</c> name (as for update sources).</summary>
    public static bool IsLocalHost(string host)
    {
        string h = host.Trim('[', ']').ToLowerInvariant();
        if (h == "localhost" || h.EndsWith(".local", StringComparison.Ordinal))
        {
            return true;
        }
        if (!IPAddress.TryParse(h, out IPAddress? ip))
        {
            return false;
        }
        if (IPAddress.IsLoopback(ip))
        {
            return true;
        }
        if (ip.AddressFamily == System.Net.Sockets.AddressFamily.InterNetworkV6)
        {
            return ip.IsIPv6LinkLocal || ip.IsIPv6UniqueLocal;
        }
        byte[] b = ip.GetAddressBytes();
        return b[0] == 10 || (b[0] == 172 && b[1] >= 16 && b[1] <= 31) || (b[0] == 192 && b[1] == 168) || (b[0] == 169 && b[1] == 254);
    }
}

/// <summary>Where the caption model stands, as one plain sentence for the log.</summary>
public sealed record CaptionModelStatus(CaptionModelState State, string Text, string? Folder = null)
{
    public bool IsReady => State == CaptionModelState.Ready;
}

public enum CaptionModelState
{
    Ready,
    Missing,
    Downloading,
    Failed,
}

/// <summary>
/// Finds the installed caption model, or installs it from the update source. The installed folder
/// is <c>&lt;Models&gt;\&lt;name&gt;</c> with every file the manifest lists and an
/// <c>archive.sha256</c> naming the archive it came from, so a build that pins a new archive
/// fetches again. A file that does not match the pinned size and SHA-256 installs nothing.
/// </summary>
public sealed class CaptionModelStore(CaptionModelManifest manifest, string modelsFolder, CaptionModelStore.Fetch? fetch = null)
{
    /// <summary>Copies or downloads <paramref name="location"/> (a path or a URL) into <paramref name="file"/>.</summary>
    public delegate Task Fetch(string location, string file, CancellationToken cancel);

    private const string Marker = "archive.sha256";
    private readonly Fetch _fetch = fetch ?? DefaultFetch;

    public CaptionModelManifest Manifest { get; } = manifest;

    /// <summary>The installed model's folder.</summary>
    public string Folder => Path.Combine(modelsFolder, Manifest.Name);

    /// <summary>The installed model, or null.</summary>
    public string? Locate()
    {
        try
        {
            string marker = Path.Combine(Folder, Marker);
            if (!File.Exists(marker) || File.ReadAllText(marker).Trim() != Manifest.Sha256 ||
                !Manifest.Files.All(f => File.Exists(Path.Combine(Folder, f))))
            {
                return null;
            }
            return Folder;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return null;
        }
    }

    /// <summary>The installed model, or an install from the update source (on the calling task).</summary>
    public async Task<CaptionModelStatus> EnsureAsync(string updateSource, CancellationToken cancel = default)
    {
        if (Locate() is { } ready)
        {
            return new CaptionModelStatus(CaptionModelState.Ready, "the caption model is installed", ready);
        }
        if (!Manifest.IsPublished)
        {
            return new CaptionModelStatus(CaptionModelState.Missing, "this build pins no published caption model");
        }
        if (updateSource.Trim().Length == 0)
        {
            return new CaptionModelStatus(CaptionModelState.Missing, "no update source is set; the caption model is fetched from its models folder");
        }
        if (Manifest.ArchiveLocation(updateSource) is not { } location)
        {
            return new CaptionModelStatus(CaptionModelState.Missing, "the update source cannot be used to find the caption model");
        }
        Directory.CreateDirectory(modelsFolder);
        string staging = Path.Combine(modelsFolder, $".incoming-{Guid.NewGuid():N}");
        string download = staging + ".zip";
        try
        {
            await _fetch(location, download, cancel).ConfigureAwait(false);
            var info = new FileInfo(download);
            if (!info.Exists || info.Length != Manifest.Bytes)
            {
                return Failed("the file's size differs from the pinned one");
            }
            if (await Sha256Async(download, cancel).ConfigureAwait(false) != Manifest.Sha256)
            {
                return Failed("the file's checksum differs from the pinned one");
            }
            try
            {
                System.IO.Compression.ZipFile.ExtractToDirectory(download, staging);
            }
            catch (InvalidDataException)
            {
                return Failed("the archive could not be unpacked");
            }
            string unpacked = Path.Combine(staging, Manifest.Name);
            if (!Manifest.Files.All(f => File.Exists(Path.Combine(unpacked, f))))
            {
                return Failed("the archive does not hold the model");
            }
            await File.WriteAllTextAsync(Path.Combine(unpacked, Marker), Manifest.Sha256, cancel).ConfigureAwait(false);
            if (Directory.Exists(Folder))
            {
                Directory.Delete(Folder, recursive: true);
            }
            Directory.Move(unpacked, Folder);
            return new CaptionModelStatus(CaptionModelState.Ready, "the caption model was installed", Folder);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or HttpRequestException or TaskCanceledException)
        {
            return Failed($"the download failed ({e.GetType().Name})");
        }
        finally
        {
            TryDelete(download);
            if (Directory.Exists(staging))
            {
                try
                {
                    Directory.Delete(staging, recursive: true);
                }
                catch (Exception e) when (e is IOException or UnauthorizedAccessException)
                {
                    // Left for the next install's cleanup to retry; never in the model's folder.
                }
            }
        }
    }

    private static CaptionModelStatus Failed(string why) => new(CaptionModelState.Failed, $"the caption model could not be installed: {why}");

    private static void TryDelete(string file)
    {
        try
        {
            File.Delete(file);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
        }
    }

    /// <summary>The file's SHA-256 as lowercase hex, streamed.</summary>
    public static async Task<string> Sha256Async(string file, CancellationToken cancel = default)
    {
        await using FileStream stream = File.OpenRead(file);
        byte[] hash = await SHA256.HashDataAsync(stream, cancel).ConfigureAwait(false);
        return Convert.ToHexStringLower(hash);
    }

    /// <summary>
    /// A path is copied; an <c>https://</c> URL, or an <c>http://</c> one to a local host, is
    /// downloaded. Redirects are followed by hand (at most 5) and only to where the first address
    /// could have pointed: never from <c>https://</c> to <c>http://</c>, never to a non-local
    /// <c>http://</c> host.
    /// </summary>
    public static async Task DefaultFetch(string location, string file, CancellationToken cancel)
    {
        if (!location.StartsWith("http://", StringComparison.OrdinalIgnoreCase) &&
            !location.StartsWith("https://", StringComparison.OrdinalIgnoreCase))
        {
            await using FileStream from = File.OpenRead(location);
            await using FileStream to = File.Create(file);
            await from.CopyToAsync(to, cancel).ConfigureAwait(false);
            return;
        }
        using var http = new HttpClient(new SocketsHttpHandler { AllowAutoRedirect = false, UseCookies = false })
        {
            Timeout = TimeSpan.FromMinutes(30),
        };
        var at = new Uri(location);
        bool secure = at.Scheme == "https";
        for (int hops = 0; ; hops++)
        {
            using HttpResponseMessage response = await http.GetAsync(at, HttpCompletionOption.ResponseHeadersRead, cancel).ConfigureAwait(false);
            if ((int)response.StatusCode is >= 300 and < 400 && response.Headers.Location is { } next)
            {
                at = next.IsAbsoluteUri ? next : new Uri(at, next);
                if (hops >= 5 || !RedirectAllowed(secure, at))
                {
                    throw new HttpRequestException("the download was redirected where it may not go");
                }
                continue;
            }
            response.EnsureSuccessStatusCode();
            await using Stream body = await response.Content.ReadAsStreamAsync(cancel).ConfigureAwait(false);
            await using FileStream to = File.Create(file);
            await body.CopyToAsync(to, cancel).ConfigureAwait(false);
            return;
        }
    }

    /// <summary>Whether a redirect may go to <paramref name="to"/>, from an https:// start or not.</summary>
    public static bool RedirectAllowed(bool fromHttps, Uri to) =>
        to.Scheme == "https" || (to.Scheme == "http" && !fromHttps && CaptionModelManifest.IsLocalHost(to.Host));
}
