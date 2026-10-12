using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace NvrView.Shell.Tests;

/// <summary>Self-update's Windows side (docs/UPDATES.md): events, the schedule on made-up times, requests, and side-by-side installs in a temporary folder.</summary>
internal static class UpdateTests
{
    private static UpdateEvent? Parse(string json) => UpdateEvent.Parse(Encoding.UTF8.GetBytes(json));

    public static void Run()
    {
        T.Run("updates: events parse", () =>
        {
            UpdateEvent e = T.NotNull(Parse(
                """{"type":"update","op":3,"action":"check","state":"available","current":"0.3.0","version":"0.4.0","notes":"Fixes.","size":10,"future":1}"""));
            T.Equal(3UL, e.Op);
            T.Equal("0.4.0", e.Version);
            T.Check(e.IsFinal);
            UpdateEvent d = T.NotNull(Parse("""{"type":"update","op":4,"action":"download","state":"downloading","received":5,"total":20}"""));
            T.Check(!d.IsFinal);
            T.Equal(5UL, d.Received);
            T.Check(Parse("""{"type":"tile_stats","tile":1}""") is null);
            T.Check(Parse("not json") is null);
        });

        T.Run("updates: the schedule waits a minute after launch, then 24 hours", () =>
        {
            var launch = new DateTimeOffset(2026, 1, 1, 12, 0, 0, TimeSpan.Zero);
            var s = new UpdateSettings();
            var schedule = new UpdateSchedule();
            T.Check(!schedule.IsDue(s, launch.AddHours(1), launch));
            s.Automatic = true;
            s.Source = @"\\server\share\nvrview";
            s.Keys.Add("nvrview-ed25519-pub 00");
            T.Check(!schedule.IsDue(s, launch.AddSeconds(30), launch));
            T.Check(schedule.IsDue(s, launch.AddSeconds(61), launch));
            s.LastCheck = launch.AddHours(-1);
            T.Check(!schedule.IsDue(s, launch.AddHours(22), launch));
            T.Check(schedule.IsDue(s, launch.AddHours(23).AddSeconds(1), launch));
            s.LastCheck = launch.AddHours(2);
            T.Check(schedule.IsDue(s, launch.AddMinutes(2), launch), "a clock set back");
            s.Automatic = false;
            T.Check(!schedule.IsDue(s, launch.AddDays(3), launch));
        });

        T.Run("updates: status follows events", () =>
        {
            DateTimeOffset now = DateTimeOffset.UnixEpoch;
            T.Equal(UpdateStatus.Kind.Checking, UpdateStatus.After(new UpdateEvent(1, "check", "started"), now, null)!.State);
            UpdateStatus d = UpdateStatus.After(new UpdateEvent(2, "download", "downloading", Received: 50, Total: 200), now, "0.4.0")!;
            T.Equal(0.25, d.Fraction);
            T.Equal("0.4.0", d.Version);
            UpdateStatus f = UpdateStatus.After(new UpdateEvent(3, "check", "failed", Reason: "bad_signature"), now, null)!;
            T.Check(f.Line("0.3.0").Contains("signature", StringComparison.Ordinal));
            foreach (string reason in new[] { "expired", "stale_manifest", "not_yet_valid", "unpublished" })
            {
                T.Check(UpdateStatus.Describe(reason, null) != UpdateStatus.Describe(null, null), reason);
            }
            T.Equal(UpdateStatus.Idle, UpdateStatus.After(new UpdateEvent(4, "check", "canceled"), now, null));
            T.Check(UpdateStatus.After(new UpdateEvent(5, "check", "later-state"), now, null) is null);
        });

        T.Run("updates: requests", () =>
        {
            var s = new UpdateSettings { Source = " https://updates.example.com/u/ ", Keys = ["k1"] };
            using JsonDocument check = JsonDocument.Parse(UpdateRequest.Json(s, "0.3.0"));
            T.Equal("https://updates.example.com/u/", check.RootElement.GetProperty("source").GetString());
            T.Check(!check.RootElement.TryGetProperty("version", out _));
            using JsonDocument dl = JsonDocument.Parse(UpdateRequest.Json(s, "0.3.0", "0.4.0", "C:/x"));
            T.Equal("C:/x", dl.RootElement.GetProperty("staging_dir").GetString());
        });

        string keyA = "nvrview-ed25519-pub " + new string('a', 32) + new string('b', 32);
        string keyB = "nvrview-ed25519-pub " + new string('c', 32) + new string('d', 32);

        T.Run("updates: bundled key and source validation", () =>
        {
            T.Equal(keyA, BundledUpdateDefaults.CanonicalKey("  nvrview-ed25519-pub  " + new string('A', 32) + new string('B', 32)));
            foreach (string bad in new[] { "", "nvrview-ed25519-pub", "nvrview-ed25519-pub abcd", "nvrview-ed25519-sec " + new string('a', 64), "nvrview-ed25519-pub " + new string('z', 64), new string('a', 64) })
            {
                T.Check(BundledUpdateDefaults.CanonicalKey(bad) is null, bad);
            }
            foreach (string good in new[] { "/Volumes/share/nvrview", "https://updates.example.com/nvrview/", "http://192.0.2.10/u", @"C:\updates", @"\\server\share\nvrview" })
            {
                T.Check(BundledUpdateDefaults.IsValidSource(good), good);
            }
            foreach (string bad in new[] { "", "relative/path", "ftp://example.com/u", "https://user:pw@example.com/u", "https://", " /x", "file:///x" })
            {
                T.Check(!BundledUpdateDefaults.IsValidSource(bad), bad);
            }
        });

        T.Run("updates: the defaults file is read leniently and from a folder", () =>
        {
            BundledUpdateDefaults d = BundledUpdateDefaults.Parse($$"""{"keys":["{{keyA}}","junk","{{keyA}}","{{keyB}}"],"source":"/srv/updates"}""");
            T.Equal(2, d.Keys.Count);
            T.Equal("/srv/updates", d.Source);
            T.Check(BundledUpdateDefaults.Parse("{not json").IsEmpty);
            T.Check(BundledUpdateDefaults.Parse("""{"keys":[],"source":"relative"}""").IsEmpty);
            T.Equal(d.Encode(), BundledUpdateDefaults.Parse(d.Encode()).Encode());
            string dir = Directory.CreateTempSubdirectory("nvrview-defaults-test").FullName;
            try
            {
                T.Check(BundledUpdateDefaults.Load(dir).IsEmpty);
                T.Check(BundledUpdateDefaults.Load(null).IsEmpty);
                File.WriteAllText(Path.Combine(dir, BundledUpdateDefaults.FileName), new BundledUpdateDefaults([keyA], "/srv/u").Encode());
                BundledUpdateDefaults back = BundledUpdateDefaults.Load(dir);
                T.Equal(keyA, back.Keys[0]);
                T.Equal("/srv/u", back.Source);
            }
            finally
            {
                Directory.Delete(dir, true);
            }
        });

        T.Run("updates: bundled defaults merge with the user's settings", () =>
        {
            var none = new UpdateSettings();
            T.Check(!none.CanCheck && none.TrustedKeys.Count == 0 && none.EffectiveSource.Length == 0 && !none.SourceIsBundled);
            var s = new UpdateSettings { Bundled = new BundledUpdateDefaults([keyA], "/srv/bundled") };
            T.Check(s.CanCheck && s.EffectiveSource == "/srv/bundled" && s.SourceIsBundled && s.SourceNote == "Bundled default");
            T.Equal("Bundled with this build", s.KeyRows[0].Note);
            using JsonDocument json = JsonDocument.Parse(UpdateRequest.Json(s, "0.3.0"));
            T.Equal("/srv/bundled", json.RootElement.GetProperty("source").GetString());
            T.Equal(keyA, json.RootElement.GetProperty("keys")[0].GetString());
            s.Source = " /srv/mine ";
            T.Check(s.EffectiveSource == "/srv/mine" && !s.SourceIsBundled && s.SourceNote.Length == 0, "the user's source wins");
            s.Source = "";
            T.Equal("/srv/bundled", s.EffectiveSource);
            s.SetBundledKey(keyA, false);
            T.Check(!s.CanCheck && s.KeyRows.Count == 1 && !s.KeyRows[0].IsOn, "turned off, not deleted");
            s.Keys = [keyB, keyA];
            T.Equal(1, s.TrustedKeys.Count);
            T.Equal(keyB, s.TrustedKeys[0]);
            s.SetBundledKey(keyA, true);
            T.Equal(2, s.TrustedKeys.Count);
            s.SetBundledKey(keyB, false);
            T.Check(s.DisabledBundledKeys.Count == 0, "a user key is not a bundled key");
        });

        T.Run("updates: only the user's choices are stored", () =>
        {
            string dir = Directory.CreateTempSubdirectory("nvrview-update-test").FullName;
            try
            {
                string file = Path.Combine(dir, "update-settings.json");
                var s = new UpdateSettings { Bundled = new BundledUpdateDefaults([keyA], "/srv/bundled"), Automatic = true };
                s.SetBundledKey(keyA, false);
                s.Save(file);
                T.Check(!File.ReadAllText(file).Contains("/srv/bundled", StringComparison.Ordinal));
                UpdateSettings back = UpdateSettings.Load(file);
                T.Check(back.Bundled.IsEmpty && back.DisabledBundledKeys.Count == 1 && back.Automatic);
                File.WriteAllText(file, """{"source":"/s","keys":["k"],"automatic":true}""");
                UpdateSettings old = UpdateSettings.Load(file);
                T.Check(old.Source == "/s" && old.Keys.Count == 1 && old.DisabledBundledKeys.Count == 0);
            }
            finally
            {
                Directory.Delete(dir, true);
            }
        });

        T.Run("updates: settings round-trip", () =>
        {
            string dir = Directory.CreateTempSubdirectory("nvrview-update-test").FullName;
            try
            {
                string file = Path.Combine(dir, "update-settings.json");
                T.Check(!UpdateSettings.Load(file).CanCheck);
                var s = new UpdateSettings { Source = "/srv/u", Keys = ["k"], Automatic = true, LastCheck = DateTimeOffset.UnixEpoch };
                s.Save(file);
                UpdateSettings back = UpdateSettings.Load(file);
                T.Equal("/srv/u", back.Source);
                T.Check(back.Automatic && back.CanCheck);
                File.WriteAllText(file, "{not json");
                T.Check(!UpdateSettings.Load(file).CanCheck);
            }
            finally
            {
                Directory.Delete(dir, true);
            }
        });

        T.Run("updates: side-by-side install next to the running folder", () =>
        {
            string root = Directory.CreateTempSubdirectory("nvrview-sbs").FullName;
            try
            {
                string app = Path.Combine(root, "NvrView-0.3.0");
                Directory.CreateDirectory(app);
                File.WriteAllText(Path.Combine(app, SideBySide.Executable), "old");
                SideBySide.RemoveLeftovers(SideBySide.FolderPath(app));
                T.Check(!Directory.Exists(Path.Combine(root, SideBySide.FolderName)), "nothing is created before a download");
                string folder = T.NotNull(SideBySide.UpdateFolder(app, out string? problem));
                T.Check(problem is null);
                T.Equal(Path.Combine(root, SideBySide.FolderName), folder);
                string staging = SideBySide.NewStagingPath(folder, "0.4.0");
                string content = Path.Combine(staging, "content");
                Directory.CreateDirectory(content);
                File.WriteAllText(Path.Combine(content, SideBySide.Executable), "new");
                string installed = SideBySide.Install(content, app, "0.4.0");
                T.Equal(Path.Combine(root, "NvrView-0.4.0"), installed);
                T.Equal("new", File.ReadAllText(Path.Combine(installed, SideBySide.Executable)));
                T.Equal("old", File.ReadAllText(Path.Combine(app, SideBySide.Executable)), "the running folder is untouched");
                T.Equal(installed, SideBySide.FindInstalled(app, "0.4.0"));
                T.Check(SideBySide.FindInstalled(app, "0.5.0") is null);

                // The name is taken: -2.
                string again = Path.Combine(folder, "staging-again", "content");
                Directory.CreateDirectory(again);
                File.WriteAllText(Path.Combine(again, SideBySide.Executable), "newer");
                T.Equal(Path.Combine(root, "NvrView-0.4.0-2"), SideBySide.Install(again, app, "0.4.0"));

                // Without the executable: refused, nothing moved.
                string empty = Path.Combine(folder, "staging-empty", "content");
                Directory.CreateDirectory(empty);
                bool refused = false;
                try
                {
                    SideBySide.Install(empty, app, "0.6.0");
                }
                catch (InvalidDataException)
                {
                    refused = true;
                }
                T.Check(refused && Directory.Exists(empty) && !Directory.Exists(Path.Combine(root, "NvrView-0.6.0")));

                SideBySide.RemoveLeftovers(folder);
                T.Equal(0, Directory.GetDirectories(folder).Length);
            }
            finally
            {
                Directory.Delete(root, true);
            }
        });

        T.Run("updates: waiting for a process to exit", () =>
        {
            T.Check(SideBySide.WaitForExit(int.MaxValue - 7, TimeSpan.FromSeconds(1)), "a process that does not exist");
            using Process self = Process.GetCurrentProcess();
            var watch = Stopwatch.StartNew();
            T.Check(!SideBySide.WaitForExit(self.Id, TimeSpan.FromMilliseconds(200)), "this process keeps running");
            T.Check(watch.Elapsed >= TimeSpan.FromMilliseconds(150));
        });
    }
}
