using System.IO.Compression;
using System.Text;

namespace NvrView.Shell.Tests;

/// <summary>The caption model's delivery (CaptionModel.cs): the manifest, where the archive is, and installs from a folder source in a temporary directory (no network).</summary>
internal static class CaptionModelTests
{
    private static readonly string[] Files = ["encoder.ort", "tokenizer.bin", "moonshine.dll"];

    public static void Run()
    {
        T.Run("caption model: the manifest parses and refuses unsafe names", () =>
        {
            string sha = "AB" + new string('0', 62);
            CaptionModelManifest m = T.NotNull(CaptionModelManifest.Parse(
                $$"""{"name":"moonshine-small-streaming-en","bytes":12,"sha256":"{{sha}}","files":["encoder.ort","../x","tokenizer.bin"],"model_arch":4,"future":1}"""));
            T.Equal("moonshine-small-streaming-en", m.Name);
            T.Equal("moonshine-small-streaming-en.zip", m.ArchiveName);
            T.Check(m.IsPublished);
            T.Equal(4u, m.Arch);
            T.Check(!(m with { Arch = 0 }).IsPublished, "a non-streaming or unknown architecture is never used");
            T.Check(m.Sha256.StartsWith("ab", StringComparison.Ordinal), "lowercase");
            T.Equal("encoder.ort,tokenizer.bin", string.Join(",", m.Files), "a path in the file list is dropped");
            T.Check(CaptionModelManifest.Parse("""{"name":"../evil","bytes":1,"sha256":""}""") is null);
            T.Check(CaptionModelManifest.Parse("not json") is null);
            T.Check(!T.NotNull(CaptionModelManifest.Parse("""{"name":"m","bytes":0,"sha256":"","model_arch":4}""")).IsPublished, "nothing pinned yet");
        });

        T.Run("caption model: the archive sits in the models folder next to the update manifest", () =>
        {
            var m = new CaptionModelManifest("m", 1, new string('a', 64), Files);
            T.Equal("https://updates.example.com/nvrview/models/m.zip", m.ArchiveLocation("https://updates.example.com/nvrview/"));
            T.Equal("https://updates.example.com/nvrview/models/m.zip", m.ArchiveLocation("https://updates.example.com/nvrview/updates.json"));
            T.Equal("https://updates.example.com/nvrview/models/m.zip", m.ArchiveLocation("https://updates.example.com/nvrview"));
            T.Check(m.ArchiveLocation("http://192.0.2.10/nvrview/") is null, "a documentation address is not on the local network");
        });

        T.Run("caption model: plain http only to a local host, never with a user name", () =>
        {
            var m = new CaptionModelManifest("m", 1, new string('a', 64), Files);
            T.Equal("http://10.0.0.5/nvrview/models/m.zip", m.ArchiveLocation("http://10.0.0.5/nvrview/")); // range test case, sanitize:allow
            T.Equal("http://nas.local/u/models/m.zip", m.ArchiveLocation("http://nas.local/u/"));
            T.Check(m.ArchiveLocation("http://updates.example.com/nvrview/") is null, "public plain http");
            T.Check(m.ArchiveLocation("https://example-user:example-password@updates.example.com/nvrview/") is null, "credentials in the URL");
            T.Check(m.ArchiveLocation("ftp://updates.example.com/") is null);
            T.Check(m.ArchiveLocation("  ") is null);
            T.Check(CaptionModelManifest.IsLocalHost("192.168.1.2") && CaptionModelManifest.IsLocalHost("172.20.0.1") && CaptionModelManifest.IsLocalHost("::1")); // range test case, sanitize:allow
            T.Check(!CaptionModelManifest.IsLocalHost("172.32.0.1") && !CaptionModelManifest.IsLocalHost("203.0.113.9")); // range test case, sanitize:allow
            T.Check(CaptionModelStore.RedirectAllowed(fromHttps: true, new Uri("https://cdn.example.com/m.zip")));
            T.Check(!CaptionModelStore.RedirectAllowed(fromHttps: true, new Uri("http://10.0.0.5/m.zip")), "never down to http"); // range test case, sanitize:allow
            T.Check(CaptionModelStore.RedirectAllowed(fromHttps: false, new Uri("http://10.0.0.5/m.zip"))); // range test case, sanitize:allow
            T.Check(!CaptionModelStore.RedirectAllowed(fromHttps: false, new Uri("http://updates.example.com/m.zip")));
        });

        T.Run("caption model: installs from a folder source after checking size and checksum", () => WithTemp(dir =>
        {
            string source = Path.Combine(dir, "source");
            (CaptionModelManifest m, string archive) = Publish(source, "m");
            var store = new CaptionModelStore(m, Path.Combine(dir, "Models"));
            T.Check(store.Locate() is null, "nothing installed yet");
            CaptionModelStatus s = store.EnsureAsync(Path.Combine(source, "updates.json")).GetAwaiter().GetResult();
            T.Equal(CaptionModelState.Ready, s.State, s.Text);
            T.Equal(store.Folder, store.Locate());
            T.Equal("model bytes encoder.ort", File.ReadAllText(Path.Combine(store.Folder, "encoder.ort")));
            T.Check(!Directory.EnumerateFileSystemEntries(Path.Combine(dir, "Models")).Any(p => Path.GetFileName(p).StartsWith(".incoming", StringComparison.Ordinal)), "no staging left");
            File.Delete(archive);
            T.Equal(CaptionModelState.Ready, store.EnsureAsync(source).GetAwaiter().GetResult().State, "installed: no second fetch");
        }));

        T.Run("caption model: a file that does not match the pin installs nothing", () => WithTemp(dir =>
        {
            string source = Path.Combine(dir, "source");
            (CaptionModelManifest m, _) = Publish(source, "m");
            var bigger = new CaptionModelStore(m with { Bytes = m.Bytes + 1 }, Path.Combine(dir, "Models"));
            CaptionModelStatus s = bigger.EnsureAsync(source).GetAwaiter().GetResult();
            T.Equal(CaptionModelState.Failed, s.State);
            T.Check(s.Text.Contains("size", StringComparison.Ordinal), s.Text);
            var other = new CaptionModelStore(m with { Sha256 = new string('0', 64) }, Path.Combine(dir, "Models"));
            CaptionModelStatus d = other.EnsureAsync(source).GetAwaiter().GetResult();
            T.Check(d.State == CaptionModelState.Failed && d.Text.Contains("checksum", StringComparison.Ordinal), d.Text);
            var missing = new CaptionModelStore(m with { Files = [.. m.Files, "decoder_kv.ort"] }, Path.Combine(dir, "Models"));
            CaptionModelStatus c = missing.EnsureAsync(source).GetAwaiter().GetResult();
            T.Check(c.State == CaptionModelState.Failed && c.Text.Contains("does not hold", StringComparison.Ordinal), c.Text);
            T.Check(!Directory.Exists(Path.Combine(dir, "Models", "m")), "nothing installed");
            T.Equal(0, Directory.GetFileSystemEntries(Path.Combine(dir, "Models")).Length, "no staging left");
        }));

        T.Run("caption model: a newly pinned archive is fetched again; no source or no pin says why", () => WithTemp(dir =>
        {
            string source = Path.Combine(dir, "source");
            (CaptionModelManifest m, _) = Publish(source, "m");
            var store = new CaptionModelStore(m, Path.Combine(dir, "Models"));
            T.Equal(CaptionModelState.Ready, store.EnsureAsync(source).GetAwaiter().GetResult().State);
            (CaptionModelManifest next, _) = Publish(source, "m", extra: "v2");
            var updated = new CaptionModelStore(next, Path.Combine(dir, "Models"));
            T.Check(updated.Locate() is null, "the old install is not the pinned archive");
            T.Equal(CaptionModelState.Ready, updated.EnsureAsync(source).GetAwaiter().GetResult().State);
            T.Equal("model bytes encoder.ort v2", File.ReadAllText(Path.Combine(updated.Folder, "encoder.ort")));
            var none = new CaptionModelStore(m, Path.Combine(dir, "Other"));
            CaptionModelStatus s = none.EnsureAsync("").GetAwaiter().GetResult();
            T.Check(s.State == CaptionModelState.Missing && s.Text.Contains("update source", StringComparison.Ordinal), s.Text);
            var unpinned = new CaptionModelStore(m with { Bytes = 0 }, Path.Combine(dir, "Other"));
            T.Equal(CaptionModelState.Missing, unpinned.EnsureAsync(source).GetAwaiter().GetResult().State);
            T.Check(!Directory.Exists(Path.Combine(dir, "Other")), "nothing created without a download");
        }));
    }

    // A models folder with <name>.zip holding <name>/<file> for every file, and its manifest.
    private static (CaptionModelManifest, string) Publish(string source, string name, string extra = "")
    {
        string models = Path.Combine(source, "models");
        Directory.CreateDirectory(models);
        string archive = Path.Combine(models, name + ".zip");
        File.Delete(archive);
        using (ZipArchive zip = ZipFile.Open(archive, ZipArchiveMode.Create))
        {
            foreach (string f in Files)
            {
                ZipArchiveEntry e = zip.CreateEntry($"{name}/{f}");
                using Stream s = e.Open();
                s.Write(Encoding.UTF8.GetBytes($"model bytes {f}{(extra.Length > 0 ? " " + extra : "")}"));
            }
        }
        string sha = CaptionModelStore.Sha256Async(archive).GetAwaiter().GetResult();
        return (new CaptionModelManifest(name, new FileInfo(archive).Length, sha, Files), archive);
    }

    private static void WithTemp(Action<string> body)
    {
        string dir = Path.Combine(Path.GetTempPath(), $"nvrview-caption-model-{Guid.NewGuid():N}");
        Directory.CreateDirectory(dir);
        try
        {
            body(dir);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }
}
