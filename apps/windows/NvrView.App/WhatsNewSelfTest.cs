// Self-test step updates-whats-new (docs/UX.md, "Release notes"): a synthetic
// release-notes.json in a throwaway app folder is read by the real Updater. A
// first run shows nothing, an upgrade shows that version's notes once, the
// same version and a downgrade show nothing, a build without the file shows
// nothing and says so in the About page, and the list holds every version
// newest first in a bounded, scrollable area. Nothing is shown on screen and
// nothing is fetched.

using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;

namespace NvrView.App;

internal static class WhatsNewSelfTest
{
    public static List<string> Run(Viewer viewer, Action<string> say)
    {
        var problems = new List<string>();
        string root = Directory.CreateTempSubdirectory("nvrview-whatsnew-selftest").FullName;
        try
        {
            string withFile = Path.Combine(root, "with", "NvrView");
            string without = Path.Combine(root, "without", "NvrView");
            Directory.CreateDirectory(withFile);
            Directory.CreateDirectory(without);
            File.WriteAllText(
                Path.Combine(withFile, BundledReleaseNotes.FileName),
                """{"releases":[{"version":"0.1.0","notes":"First made-up notes."},""" +
                """{"version":"0.3.0","notes":"Newest made-up notes.\n- a bullet\n- another bullet"},""" +
                """{"version":"0.2.0","notes":"Middle made-up notes."}]}""");
            string settings = Path.Combine(root, "settings.json");
            Updater Make(string appDir, string version) =>
                new(viewer, null!, settings, automaticAllowed: false, appDir: appDir, current: version);

            // 1. A first run, then an upgrade, then the same version, then a downgrade.
            Updater first = Make(withFile, "0.2.0");
            if (string.Join(",", first.ReleaseNotes.Releases.Select(r => r.Version)) != "0.3.0,0.2.0,0.1.0")
            {
                problems.Add("whats-new: the bundled notes were not read newest first");
            }
            if (first.TakeWhatsNew() is not null)
            {
                problems.Add("whats-new: a first run showed the notes");
            }
            Updater upgraded = Make(withFile, "0.3.0");
            ReleaseNote? shown = upgraded.TakeWhatsNew();
            if (shown?.Version != "0.3.0" || !shown.Notes.StartsWith("Newest made-up notes.", StringComparison.Ordinal))
            {
                problems.Add("whats-new: an upgrade did not show the new version's notes");
            }
            if (Make(withFile, "0.3.0").TakeWhatsNew() is not null)
            {
                problems.Add("whats-new: the notes were shown twice");
            }
            if (Make(withFile, "0.2.0").TakeWhatsNew() is not null)
            {
                problems.Add("whats-new: a downgrade showed notes");
            }
            if (Make(withFile, "0.3.0").TakeWhatsNew()?.Version != "0.3.0")
            {
                problems.Add("whats-new: going up again after a downgrade did not show the notes");
            }

            // 2. The list: every version, newest first, bullets as bullets, bounded and scrollable.
            string list = upgraded.ReleaseNotes.DisplayText;
            int a = list.IndexOf("Version 0.3.0", StringComparison.Ordinal);
            int b = list.IndexOf("Version 0.2.0", StringComparison.Ordinal);
            int c = list.IndexOf("Version 0.1.0", StringComparison.Ordinal);
            if (a < 0 || b < 0 || c < 0)
            {
                problems.Add("whats-new: the list lacks a version");
            }
            else if (!(a < b && b < c))
            {
                problems.Add("whats-new: the list is not newest first");
            }
            if (!list.Contains("• a bullet", StringComparison.Ordinal) || list.Contains("- a bullet", StringComparison.Ordinal))
            {
                problems.Add("whats-new: a \"- \" line is not shown as a bullet");
            }
            ScrollViewer view = UpdateNotesView.Notes(string.Concat(Enumerable.Repeat(list + "\n", 6)), UpdateNotesView.ListMaxHeight);
            if (view.MaxHeight > UpdateNotesView.ListMaxHeight || view.VerticalScrollBarVisibility == ScrollBarVisibility.Disabled)
            {
                problems.Add("whats-new: the list is not bounded and scrollable");
            }

            // 3. A build without the file: nothing shown, nothing to list, and a version without notes shows no window.
            Updater bare = Make(without, "0.4.0");
            if (bare.TakeWhatsNew() is not null || !bare.ReleaseNotes.IsEmpty)
            {
                problems.Add("whats-new: a build without the file shows notes");
            }
            if (Make(withFile, "0.4.0").TakeWhatsNew() is not null)
            {
                problems.Add("whats-new: a version without notes showed a window");
            }
            say($"whats-new: upgrade shown {shown is not null}, {problems.Count} problems");
        }
        finally
        {
            Directory.Delete(root, true);
        }
        return problems;
    }
}
