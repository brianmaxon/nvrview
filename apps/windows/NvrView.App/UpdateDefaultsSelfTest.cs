// Self-test step updates-bundled-defaults (docs/UPDATES.md, "Bundled
// defaults"): an update-defaults.json in a throwaway app folder, with a
// throwaway key and a throwaway folder as the source, is read by the real
// Updater; the key is trusted and the source used, Settings' rows show both
// as bundled, the user's choices override and are the only thing stored, and
// with no file nothing is trusted. Nothing is fetched. The keys are the
// public test vectors of RFC 8032 (valid Ed25519 public keys with no secret
// that matters), since .NET cannot generate Ed25519 keys; written in pieces
// so a secret scanner does not take them for one.

using System.Text.Json;
using NvrView.Shell;

namespace NvrView.App;

internal static class UpdateDefaultsSelfTest
{
    private const string KeyHex = "d75a980182b10ab7" + "d54bfed3c964073a" + "0ee172f3daa62325" + "af021a68f707511a";
    private const string OtherHex = "3d4017c3e843895a" + "92b70aa74d1b7ebc" + "9c982ccf2ec4968c" + "c0cd55f12af4660c";

    public static List<string> Run(Viewer viewer, Action<string> say)
    {
        var problems = new List<string>();
        string root = Directory.CreateTempSubdirectory("nvrview-updates-selftest").FullName;
        try
        {
            string withFile = Path.Combine(root, "with", "NvrView");
            string without = Path.Combine(root, "without", "NvrView");
            string source = Path.Combine(root, "source");
            Directory.CreateDirectory(withFile);
            Directory.CreateDirectory(without);
            Directory.CreateDirectory(source);
            string key = "nvrview-ed25519-pub " + KeyHex;
            string other = "nvrview-ed25519-pub " + OtherHex;
            File.WriteAllText(Path.Combine(withFile, BundledUpdateDefaults.FileName), new BundledUpdateDefaults([key], source).Encode());

            Updater Make(string appDir, string settingsName) =>
                new(viewer, null!, Path.Combine(root, settingsName), automaticAllowed: false, appDir: appDir);
            (string Source, string[] Keys) Request(Updater u)
            {
                using JsonDocument d = JsonDocument.Parse(UpdateRequest.Json(u.Settings, "0.3.0"));
                return (d.RootElement.GetProperty("source").GetString()!, [.. d.RootElement.GetProperty("keys").EnumerateArray().Select(k => k.GetString()!)]);
            }

            // 1. A bundled file: the key is trusted and the source used, both listed as bundled.
            Updater a = Make(withFile, "a.json");
            (string requestSource, string[] requestKeys) = Request(a);
            bool bundledUsed = requestSource == source && requestKeys.SequenceEqual([key]) && a.Settings.SourceIsBundled;
            if (!a.Settings.CanCheck)
            {
                problems.Add("updates: a bundled key and source did not allow a check");
            }
            if (!bundledUsed)
            {
                problems.Add("updates: the check request does not carry the bundled key and source");
            }
            if (a.Settings.KeyRows.Count != 1 || a.Settings.KeyRows[0].Note != "Bundled with this build")
            {
                problems.Add("updates: Settings does not list the key as bundled with this build");
            }
            if (a.Settings.SourceNote != "Bundled default")
            {
                problems.Add("updates: Settings does not show the source as the bundled default");
            }
            if (Updater.KeyId(key) == "?")
            {
                problems.Add("updates: the core could not read the bundled key");
            }

            // 2. The user's choices win. A bundled key is turned off, never deleted; a user key counts alone.
            a.SetBundledKey(key, false);
            if (a.Settings.CanCheck || a.Settings.KeyRows.Count != 1)
            {
                problems.Add("updates: a bundled key turned off still trusts or vanished");
            }
            if (a.AddKey(other) is not null)
            {
                problems.Add("updates: a user key was refused");
            }
            if (!Request(a).Keys.SequenceEqual([other]))
            {
                problems.Add("updates: a turned-off bundled key was still sent");
            }
            a.RemoveKey(key);
            if (!a.Settings.KeyRows[0].IsBundled)
            {
                problems.Add("updates: Remove deleted a bundled key");
            }
            string mine = Path.Combine(root, "mine");
            if (a.SetSource(mine) is not null)
            {
                problems.Add("updates: a folder source was refused");
            }
            if (Request(a).Source != mine || a.Settings.SourceIsBundled)
            {
                problems.Add("updates: the user's source did not override the bundled one");
            }
            Updater again = Make(withFile, "a.json");
            if (again.Settings.Source != mine || !again.Settings.Keys.SequenceEqual([other]) || !again.Settings.DisabledBundledKeys.SequenceEqual([key]))
            {
                problems.Add("updates: the user's choices were not kept");
            }
            if (!again.Settings.Bundled.Keys.SequenceEqual([key]))
            {
                problems.Add("updates: the bundled key was not read again at the next start");
            }
            _ = again.SetSource("");
            if (Request(again).Source != source || !again.Settings.SourceIsBundled)
            {
                problems.Add("updates: clearing the source did not return to the bundled default");
            }

            // 3. No file: nothing is trusted and nothing is checked.
            Updater b = Make(without, "b.json");
            (string noSource, string[] noKeys) = Request(b);
            if (b.Settings.CanCheck || !b.Settings.Bundled.IsEmpty || b.Settings.KeyRows.Count != 0 || noSource.Length != 0 || noKeys.Length != 0)
            {
                problems.Add("updates: a build without a defaults file trusts or uses something");
            }
            say($"updates: bundled key and source used {bundledUsed}, no file trusts nothing {!b.Settings.CanCheck}, {problems.Count} problems");
        }
        finally
        {
            Directory.Delete(root, true);
        }
        return problems;
    }
}
