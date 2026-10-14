namespace NvrView.Shell.Tests;

/// <summary>Bundled release notes and the "What's New" decision (docs/UPDATES.md, "Bundled release notes"; docs/UX.md, "Release notes"). Mirrors NVRCoreTests/ReleaseNotesTests.swift. All text is made up.</summary>
internal static class ReleaseNotesTests
{
    private const string Json =
        """{"releases":[{"version":"0.2.0","notes":"Two."},{"version":"0.10.0","notes":"Ten.\n- x"},""" +
        """{"version":"0.9.1","notes":"Nine."},{"version":"bad","notes":"No."},{"version":"0.9.1","notes":"Dup."},""" +
        """{"version":"0.5.0","notes":"   "},{"version":"0.6.0"}]}""";

    public static void Run()
    {
        T.Run("release notes: version order", () =>
        {
            T.Equal(1, BundledOrder("0.10.0", "0.9.9"));
            T.Equal(0, BundledOrder("1.0", "1.0.0"));
            T.Equal(-1, BundledOrder("0.3.0", "0.3.1"));
            T.Equal(0, BundledOrder("1.2.3-beta", "1.2.3"));
            foreach (string bad in new[] { "", "x", "1..2", "1.a", "1.2.3.4.5", "-1" })
            {
                T.Check(AppVersion.Parts(bad) is null, bad);
            }
        });

        T.Run("version: read from version.txt next to the executable, never compiled in", () =>
        {
            string dir = Path.Combine(Path.GetTempPath(), "nvrview-version-tests-" + Guid.NewGuid().ToString("N")[..8]);
            Directory.CreateDirectory(dir);
            try
            {
                T.Equal("0.0.0", AppVersion.Read(dir));
                File.WriteAllText(Path.Combine(dir, AppVersion.FileName), "1.4.2\n");
                T.Equal("1.4.2", AppVersion.Read(dir));
                File.WriteAllText(Path.Combine(dir, AppVersion.FileName), "not a version");
                T.Equal("0.0.0", AppVersion.Read(dir));
                T.Equal("0.0.0", AppVersion.Read(Path.Combine(dir, "missing")));
            }
            finally
            {
                Directory.Delete(dir, recursive: true);
            }
        });

        T.Run("release notes: the file is read leniently, newest first", () =>
        {
            BundledReleaseNotes n = BundledReleaseNotes.Parse(Json);
            T.Equal("0.10.0,0.9.1,0.2.0", string.Join(",", n.Releases.Select(r => r.Version)));
            T.Equal("Nine.", n.Releases[1].Notes);
            T.Equal("Two.", T.NotNull(n.NotesFor("0.2")).Notes);
            foreach (string bad in new[] { "", "{not json", "[]", """{"releases":5}""", """{"releases":[1,"x"]}""" })
            {
                T.Check(BundledReleaseNotes.Parse(bad).IsEmpty, bad);
            }
            string cut = BundledReleaseNotes.Parse("""{"releases":[{"version":"1.0.0","notes":"%"}]}""".Replace("%", new string('a', 2500), StringComparison.Ordinal)).Releases[0].Notes;
            T.Equal(2000, cut.Length);
        });

        T.Run("release notes: a missing file is nothing", () =>
        {
            T.Check(BundledReleaseNotes.Load(Path.Combine(Path.GetTempPath(), "nvrview-no-such-" + Guid.NewGuid().ToString("N"))).IsEmpty);
            T.Check(BundledReleaseNotes.Load(null).IsEmpty);
        });

        T.Run("release notes: plain text with bullets", () =>
        {
            T.Equal("Intro.\n\n• one\n• two\n  • nested", ReleaseNotesText.Format("Intro.\r\n\r\n\r\n\r\n- one  \n* two\n  - nested"));
            string text = BundledReleaseNotes.Parse(Json).DisplayText;
            T.Check(text.StartsWith("Version 0.10.0\n\nTen.\n• x", StringComparison.Ordinal));
            T.Check(text.Contains("Version 0.2.0\n\nTwo.", StringComparison.Ordinal));
        });

        T.Run("release notes: what's new decisions", () =>
        {
            BundledReleaseNotes b = BundledReleaseNotes.Parse(Json);
            // Upgrade: notes, and the new version is remembered.
            T.Equal(new WhatsNew.Decision(b.Releases[0], "0.10.0"), WhatsNew.Decide("0.10.0", "0.9.1", b));
            // Fresh install: no window, remember.
            T.Equal(new WhatsNew.Decision(null, "0.10.0"), WhatsNew.Decide("0.10.0", null, b));
            // Same version: nothing at all.
            T.Equal(new WhatsNew.Decision(null, null), WhatsNew.Decide("0.10.0", "0.10.0", b));
            // Downgrade: no window, remember the older one so going up shows it again.
            T.Equal(new WhatsNew.Decision(null, "0.2.0"), WhatsNew.Decide("0.2.0", "0.10.0", b));
            // An upgrade to a version the file lacks, and a build with no file.
            T.Equal(new WhatsNew.Decision(null, "0.11.0"), WhatsNew.Decide("0.11.0", "0.10.0", b));
            T.Equal(new WhatsNew.Decision(null, "0.11.0"), WhatsNew.Decide("0.11.0", "0.10.0", BundledReleaseNotes.None));
            // A stored value that is not a version counts as a first run; a development version does nothing.
            T.Equal(new WhatsNew.Decision(null, "0.10.0"), WhatsNew.Decide("0.10.0", "junk", b));
            T.Equal(new WhatsNew.Decision(null, null), WhatsNew.Decide("dev", "0.1.0", b));
        });

        T.Run("release notes: the last version that ran is stored with the update settings", () =>
        {
            string file = Path.Combine(Path.GetTempPath(), "nvrview-lastrun-" + Guid.NewGuid().ToString("N") + ".json");
            try
            {
                T.Check(UpdateSettings.Load(file).LastRunVersion is null);
                new UpdateSettings { LastRunVersion = "0.3.0" }.Save(file);
                T.Equal("0.3.0", UpdateSettings.Load(file).LastRunVersion);
            }
            finally
            {
                File.Delete(file);
            }
        });
    }

    private static int? BundledOrder(string a, string b) => AppVersion.Compare(a, b);
}
