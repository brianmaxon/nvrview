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
