// Call transcripts saved as text files (docs/UX.md "Doorbell calls"), only when the user turned
// "Save call transcripts" on. The decisions are pure and tested headless, with the same vectors as
// the macOS app's TranscriptFileTests (apps/macos NVRCoreTests): the file's name and text, a name
// that does not collide, and which old files to delete. The app does the writing (atomic) and the
// deleting.

using System.Globalization;
using System.Text;

namespace NvrView.Tiles;

/// <summary>A file in the transcripts folder, for <see cref="TranscriptFiles.ToDelete"/>.</summary>
public sealed record StoredTranscript(string Name, long Bytes, DateTime Modified);

/// <summary>A saved call transcript: names, text, and the folder's bounds.</summary>
public static class TranscriptFiles
{
    /// <summary>At most this many transcript files are kept, newest first.</summary>
    public const int MaxFiles = 500;

    /// <summary>And at most this many bytes in all (50 MB).</summary>
    public const long MaxBytes = 50L * 1024 * 1024;

    /// <summary>The camera name's share of a file name, in Unicode scalars.</summary>
    public const int MaxCameraName = 60;

    private const string Reserved = "/\\:*?\"<>|";

    /// <summary>
    /// The file's name: "2026-10-11 17.02.03 Front door.txt", the call's start in
    /// <paramref name="zone"/>, then the camera's name made safe for a file name on Windows and macOS.
    /// </summary>
    public static string FileName(DateTime startedAt, string camera, TimeZoneInfo zone) =>
        $"{Local(startedAt, zone).ToString("yyyy-MM-dd HH.mm.ss", CultureInfo.InvariantCulture)} {Sanitized(camera)}.txt";

    /// <summary>
    /// A camera name fit for a file name: no path separators, reserved or control characters (each
    /// becomes "-"), whitespace runs as one space, no leading or trailing spaces or dots, at most
    /// <see cref="MaxCameraName"/> scalars; "Camera" when nothing is left.
    /// </summary>
    public static string Sanitized(string name)
    {
        var runes = new List<Rune>();
        bool space = false;
        foreach (Rune r in name.EnumerateRunes())
        {
            if (Rune.IsWhiteSpace(r))
            {
                space = true;
                continue;
            }
            if (space && runes.Count > 0)
            {
                runes.Add(new Rune(' '));
            }
            space = false;
            bool reserved = r.IsAscii && Reserved.Contains((char)r.Value, StringComparison.Ordinal);
            runes.Add(reserved || r.Value < 0x20 || r.Value == 0x7F ? new Rune('-') : r);
        }
        var sb = new StringBuilder();
        foreach (Rune r in runes.Take(MaxCameraName))
        {
            sb.Append(r.ToString());
        }
        string s = sb.ToString().Trim(' ', '.');
        return s.Length == 0 ? "Camera" : s;
    }

    /// <summary>
    /// <paramref name="name"/>, or "name 2.txt", "name 3.txt", ... if a file of that name is in
    /// <paramref name="taken"/> (compared without regard to case, as both systems' default file
    /// systems do).
    /// </summary>
    public static string UniqueName(string name, IEnumerable<string> taken)
    {
        var lower = new HashSet<string>(taken.Select(t => t.ToLowerInvariant()), StringComparer.Ordinal);
        if (!lower.Contains(name.ToLowerInvariant()))
        {
            return name;
        }
        string stem = name.EndsWith(".txt", StringComparison.Ordinal) ? name[..^4] : name;
        int n = 2;
        while (lower.Contains($"{stem} {n}.txt".ToLowerInvariant()))
        {
            n++;
        }
        return $"{stem} {n}.txt";
    }

    /// <summary>
    /// The file's text: a header (camera, kind of call, start and end in <paramref name="zone"/>
    /// with its offset), a blank line, then each finished caption line with its time and side
    /// ("17:02:10 Visitor: Hello there."). Lines end with "\n".
    /// </summary>
    public static string Text(string camera, int presses, DateTime startedAt, DateTime endedAt, IEnumerable<TranscriptLine> lines, TimeZoneInfo zone)
    {
        CultureInfo inv = CultureInfo.InvariantCulture;
        long seconds = Math.Max(0, (long)Math.Round((endedAt - startedAt).TotalSeconds, MidpointRounding.AwayFromZero));
        string kind = presses == 0 ? "conversation started in the app" : $"doorbell, {presses} press{(presses == 1 ? "" : "es")}";
        var sb = new StringBuilder();
        sb.Append("NVRView call transcript\n");
        sb.Append(inv, $"Camera: {OneLine(camera)}\n");
        sb.Append(inv, $"Call: {kind}\n");
        sb.Append(inv, $"Started: {Local(startedAt, zone).ToString("yyyy-MM-dd HH:mm:ss", inv)} {Offset(zone, startedAt)}\n");
        sb.Append(inv, $"Ended: {Local(endedAt, zone).ToString("yyyy-MM-dd HH:mm:ss", inv)} ({seconds} s)\n");
        sb.Append('\n');
        foreach (TranscriptLine line in lines)
        {
            string time = line.At is { } at ? Local(at, zone).ToString("HH:mm:ss", inv) : "--:--:--";
            sb.Append(inv, $"{time} {TranscriptLine.Label(line.Source)}: {OneLine(line.Text)}\n");
        }
        return sb.ToString();
    }

    /// <summary>
    /// The files to delete, oldest first, so that the newest ones kept are at most
    /// <paramref name="maxFiles"/> and <paramref name="maxBytes"/> together (the newest one is always
    /// kept). Newest by modification time, then by name.
    /// </summary>
    public static IReadOnlyList<string> ToDelete(IEnumerable<StoredTranscript> files, int maxFiles = MaxFiles, long maxBytes = MaxBytes)
    {
        StoredTranscript[] newestFirst = [.. files.OrderByDescending(f => f.Modified).ThenByDescending(f => f.Name, StringComparer.Ordinal)];
        int kept = 0;
        long bytes = 0;
        int cut = newestFirst.Length;
        for (int i = 0; i < newestFirst.Length; i++)
        {
            if (kept > 0 && (kept >= maxFiles || bytes + newestFirst[i].Bytes > maxBytes))
            {
                cut = i;
                break;
            }
            kept++;
            bytes += newestFirst[i].Bytes;
        }
        return [.. newestFirst[cut..].Reverse().Select(f => f.Name)];
    }

    /// <summary>"UTC", "UTC+05:30", "UTC-04:00": the zone's offset at <paramref name="at"/>.</summary>
    public static string Offset(TimeZoneInfo zone, DateTime at)
    {
        TimeSpan o = zone.GetUtcOffset(ToUtc(at));
        if (o == TimeSpan.Zero)
        {
            return "UTC";
        }
        int minutes = (int)Math.Abs(o.TotalMinutes);
        return string.Create(CultureInfo.InvariantCulture, $"UTC{(o < TimeSpan.Zero ? "-" : "+")}{minutes / 60:00}:{minutes % 60:00}");
    }

    private static readonly char[] Newlines = ['\n', '\r', '\v', '\f', '\u0085', '\u2028', '\u2029'];

    private static string OneLine(string s) => string.Join(" ", s.Split(Newlines, StringSplitOptions.RemoveEmptyEntries));

    private static DateTime ToUtc(DateTime at) => at.Kind == DateTimeKind.Utc ? at : at.ToUniversalTime();

    private static DateTime Local(DateTime at, TimeZoneInfo zone) => TimeZoneInfo.ConvertTimeFromUtc(ToUtc(at), zone);
}
