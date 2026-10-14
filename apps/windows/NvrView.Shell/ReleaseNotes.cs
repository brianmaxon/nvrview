// The release notes a build bundles and the "What's New" decision
// (docs/UPDATES.md, "Bundled release notes"; docs/UX.md, "Release notes").
// Plain .NET, tested by NvrView.Shell.Tests; mirrors NVRCore's
// ReleaseNotes.swift on macOS.

using System.Text.Json;
using System.Text.Json.Nodes;

namespace NvrView.Shell;

/// <summary>One version's release notes.</summary>
public sealed record ReleaseNote(string Version, string Notes);

/// <summary>Dotted version numbers, as in VERSION ("1.2.3"; a "-suffix" or "+build" is ignored for the order).</summary>
public static class AppVersion
{
    /// <summary>The file next to the executable that holds the version (written by scripts/windows-app.sh when the app is built or packaged; a version bump changes this file only, so the compiled code is the tested code).</summary>
    public const string FileName = "version.txt";

    /// <summary>The version in <see cref="FileName"/> in the folder, or "0.0.0" when the file is missing or is not a version.</summary>
    public static string Read(string folder)
    {
        try
        {
            string text = File.ReadAllText(Path.Combine(folder, FileName)).Trim();
            if (Parts(text) is not null)
            {
                return text;
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
        }
        return "0.0.0";
    }

    /// <summary>This app's version, read once from the folder it runs from.</summary>
    public static string Installed { get; } = Read(AppContext.BaseDirectory);

    /// <summary>The numeric parts, or null when the text is not dotted numbers.</summary>
    public static int[]? Parts(string text)
    {
        string trimmed = text.Trim();
        int cut = trimmed.IndexOfAny(['-', '+']);
        string core = cut < 0 ? trimmed : trimmed[..cut];
        string[] pieces = core.Split('.');
        if (pieces.Length is 0 or > 4)
        {
            return null;
        }
        var parts = new int[pieces.Length];
        for (int i = 0; i < pieces.Length; i++)
        {
            if (pieces[i].Length == 0 || !pieces[i].All(char.IsAsciiDigit) ||
                !int.TryParse(pieces[i], System.Globalization.NumberStyles.None, System.Globalization.CultureInfo.InvariantCulture, out parts[i]))
            {
                return null;
            }
        }
        return parts;
    }

    /// <summary>-1, 0, or 1; null when either is not a version. Missing parts count as 0.</summary>
    public static int? Compare(string a, string b)
    {
        if (Parts(a) is not { } x || Parts(b) is not { } y)
        {
            return null;
        }
        for (int i = 0; i < Math.Max(x.Length, y.Length); i++)
        {
            int l = i < x.Length ? x[i] : 0;
            int r = i < y.Length ? y[i] : 0;
            if (l != r)
            {
                return l < r ? -1 : 1;
            }
        }
        return 0;
    }
}

/// <summary>The release notes a build bundles: release-notes.json next to the executable, written by the packaging scripts from notes kept outside the repository. A development build has none.</summary>
public sealed class BundledReleaseNotes
{
    public const string FileName = "release-notes.json";

    /// <summary>The most characters kept of one version's notes (the manifest's limit).</summary>
    public const int MaxCharacters = 2000;

    public static readonly BundledReleaseNotes None = new([]);

    public BundledReleaseNotes(IReadOnlyList<ReleaseNote> releases) => Releases = releases;

    /// <summary>Newest first.</summary>
    public IReadOnlyList<ReleaseNote> Releases { get; }

    public bool IsEmpty => Releases.Count == 0;

    public ReleaseNote? NotesFor(string version) => Releases.FirstOrDefault(r => AppVersion.Compare(r.Version, version) == 0);

    /// <summary>Reads the file. Lenient: malformed JSON gives nothing; an entry with a bad version or no text is skipped, a repeated version keeps its first entry, and the result is ordered newest first whatever the file's order.</summary>
    public static BundledReleaseNotes Parse(string json)
    {
        try
        {
            if (JsonNode.Parse(json) is not JsonObject o || o["releases"] is not JsonArray list)
            {
                return None;
            }
            var found = new List<ReleaseNote>();
            foreach (JsonNode? node in list)
            {
                if (node is not JsonObject entry ||
                    entry["version"] is not JsonValue vv || !vv.TryGetValue(out string? rawVersion) ||
                    entry["notes"] is not JsonValue nv || !nv.TryGetValue(out string? rawNotes))
                {
                    continue;
                }
                string version = rawVersion.Trim();
                string text = ReleaseNotesText.Clean(rawNotes);
                if (AppVersion.Parts(version) is null || text.Length == 0 || found.Any(r => AppVersion.Compare(r.Version, version) == 0))
                {
                    continue;
                }
                found.Add(new ReleaseNote(version, text));
            }
            found.Sort((a, b) => AppVersion.Compare(b.Version, a.Version) ?? 0);
            return new BundledReleaseNotes(found);
        }
        catch (Exception e) when (e is JsonException or InvalidOperationException or FormatException)
        {
            return None;
        }
    }

    /// <summary>The notes file in <paramref name="directory"/>, or none.</summary>
    public static BundledReleaseNotes Load(string? directory)
    {
        if (directory is null)
        {
            return None;
        }
        try
        {
            string file = Path.Combine(directory, FileName);
            return File.Exists(file) && new FileInfo(file).Length <= 1024 * 1024 ? Parse(File.ReadAllText(file)) : None;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return None;
        }
    }

    /// <summary>Every version's notes as one text, newest first (Help, Release Notes).</summary>
    public string DisplayText =>
        string.Join("\n\n\n", Releases.Select(r => $"Version {r.Version}\n\n{ReleaseNotesText.Format(r.Notes)}"));
}

/// <summary>Release notes are plain text: lines, and "- " bullets.</summary>
public static class ReleaseNotesText
{
    /// <summary>Normalizes line endings, trims trailing spaces, and cuts to the limit.</summary>
    public static string Clean(string raw)
    {
        string[] lines = raw.Replace("\r\n", "\n", StringComparison.Ordinal).Replace('\r', '\n').Split('\n');
        string text = string.Join("\n", lines.Select(l => l.TrimEnd(' ', '\t'))).Trim();
        return text.Length > BundledReleaseNotes.MaxCharacters ? text[..BundledReleaseNotes.MaxCharacters] : text;
    }

    /// <summary>The text as shown: a "- " or "* " bullet becomes a bullet character, and runs of blank lines shrink to one.</summary>
    public static string Format(string notes)
    {
        var output = new List<string>();
        bool blank = false;
        foreach (string line in Clean(notes).Split('\n'))
        {
            if (line.Length == 0)
            {
                if (!blank)
                {
                    output.Add("");
                }
                blank = true;
                continue;
            }
            blank = false;
            string rest = line.TrimStart(' ');
            string indent = line[..(line.Length - rest.Length)];
            output.Add(rest.StartsWith("- ", StringComparison.Ordinal) || rest.StartsWith("* ", StringComparison.Ordinal)
                ? indent + "• " + rest[2..].Trim()
                : line);
        }
        return string.Join("\n", output);
    }
}

/// <summary>How the notes view styles one line of formatted notes.</summary>
public enum NoteLineKind
{
    Blank,
    Heading,
    Bullet,
    Text,
}

/// <summary>One line of formatted notes with its style; <see cref="Level"/> is the bullet's indent in steps of two spaces.</summary>
public readonly record struct NoteLine(string Text, NoteLineKind Kind, int Level);

public static class ReleaseNotesLines
{
    /// <summary>The longest line that can be a heading.</summary>
    public const int MaxHeadingLength = 60;

    /// <summary>
    /// Classifies each line of <see cref="ReleaseNotesText.Format"/> output. A heading is a short line that starts a
    /// block (first line, or after a blank line), does not end like a sentence, and is followed by a bullet, a blank
    /// line, or the end. Mirrors ReleaseNotesText.lineKinds in NVRCore.
    /// </summary>
    public static IReadOnlyList<NoteLine> Classify(string formatted)
    {
        string[] lines = formatted.Split('\n');
        static bool IsBullet(string l) => l.TrimStart(' ').StartsWith("• ", StringComparison.Ordinal);
        var result = new List<NoteLine>();
        for (int i = 0; i < lines.Length; i++)
        {
            string line = lines[i];
            if (line.Length == 0)
            {
                result.Add(new NoteLine(line, NoteLineKind.Blank, 0));
            }
            else if (IsBullet(line))
            {
                result.Add(new NoteLine(line.TrimStart(' '), NoteLineKind.Bullet, (line.Length - line.TrimStart(' ').Length) / 2));
            }
            else
            {
                bool startsBlock = i == 0 || lines[i - 1].Length == 0;
                bool nextOk = i + 1 >= lines.Length || lines[i + 1].Length == 0 || IsBullet(lines[i + 1]);
                bool sentence = ".,;!?".Contains(line[^1], StringComparison.Ordinal);
                bool heading = startsBlock && nextOk && !sentence && line.Length <= MaxHeadingLength && line[0] != ' ';
                result.Add(new NoteLine(line, heading ? NoteLineKind.Heading : NoteLineKind.Text, 0));
            }
        }
        return result;
    }
}

/// <summary>What to do about the "What's New" window at launch.</summary>
public static class WhatsNew
{
    /// <param name="Show">The notes to show now, or null.</param>
    /// <param name="Remember">The version to store as the last one that ran, or null to leave it.</param>
    public sealed record Decision(ReleaseNote? Show, string? Remember);

    /// <summary>
    /// A first run (nothing stored) shows nothing and remembers this version. A newer version than the stored
    /// one shows its notes, if the build has them, and remembers it. The same version shows nothing; an older
    /// one (a downgrade) shows nothing and remembers the older one, so going up again shows the notes. A
    /// current version that is not a version (a development build) does nothing at all.
    /// </summary>
    public static Decision Decide(string current, string? lastRun, BundledReleaseNotes bundled)
    {
        if (AppVersion.Parts(current) is null)
        {
            return new Decision(null, null);
        }
        string now = current.Trim();
        if (lastRun is null || AppVersion.Compare(now, lastRun) is not { } order)
        {
            return new Decision(null, now);
        }
        return order > 0 ? new Decision(bundled.NotesFor(now), now) : new Decision(null, lastRun == now ? null : now);
    }
}
