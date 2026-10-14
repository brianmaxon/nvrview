// The Recordings window's Export tab, the decisions (docs/UX.md "Recordings", "Export tab"): which kinds of export
// apply to what is open, the default, which options show for a kind, the summary line, and what stops an export before
// it starts. Pure, so both apps share the vectors (fixtures/synthetic/export-panel.json); the same type as the macOS
// app's ExportPanel.swift (NVRCore), with the same tests. The panel draws these answers and runs the commands
// ExportController already has.

using System.Globalization;

namespace NvrView.Shell;

/// <summary>What the Export button writes.</summary>
public enum ExportKind
{
    /// <summary>The range cut from the keyframe before it (a copy, instant).</summary>
    TrimCopy,

    /// <summary>The range from the exact picture (re-encoded).</summary>
    TrimExact,

    /// <summary>The follow cut as one video.</summary>
    FollowCut,

    /// <summary>The 2-up grid as one video.</summary>
    Grid2,

    /// <summary>The 4-up grid as one video.</summary>
    Grid4,

    /// <summary>The device's files for the range, as they are.</summary>
    Download,

    /// <summary>The picture shown now.</summary>
    StillCurrent,

    /// <summary>One picture per camera.</summary>
    StillEach,
}

/// <summary>Which stream a download (or the download an export makes first) reads.</summary>
public enum ExportQuality
{
    /// <summary>The main stream.</summary>
    Main,

    /// <summary>The sub-stream.</summary>
    Sub,
}

/// <summary>What is open when the panel decides.</summary>
/// <param name="HasRange">An incident or a camera is playing, so there is a range to export.</param>
/// <param name="HasPictures">A device playback or the follow cut or grid is open, so there are pictures.</param>
/// <param name="Editable">The incident is downloaded, so a follow cut and grids can be made of its files.</param>
/// <param name="CanWriteEdit">The app can write a follow cut or grid.</param>
/// <param name="SourceCameras">The cameras the range or the incident has.</param>
/// <param name="CheckedCameras">The cameras checked in the list.</param>
public sealed record ExportContext(
    bool HasRange, bool HasPictures, bool Editable, bool CanWriteEdit, int SourceCameras, int CheckedCameras);

/// <summary>The rules of the Export tab (see the file comment).</summary>
public static class ExportPanel
{
    /// <summary>The shortest range an export may cover (the points' own minimum).</summary>
    public const long MinimumMs = InOutPoints.MinimumMs;

    private const long DayMs = 86_400_000;

    /// <summary>The ids the shared vectors use.</summary>
    public static string Id(this ExportKind kind) => kind switch
    {
        ExportKind.TrimCopy => "trim-copy",
        ExportKind.TrimExact => "trim-exact",
        ExportKind.FollowCut => "follow-cut",
        ExportKind.Grid2 => "grid-2",
        ExportKind.Grid4 => "grid-4",
        ExportKind.Download => "download",
        ExportKind.StillCurrent => "still-current",
        _ => "still-each",
    };

    /// <summary>The kind an id names, or null.</summary>
    public static ExportKind? KindOf(string id) =>
        Enum.GetValues<ExportKind>().Cast<ExportKind?>().FirstOrDefault(k => k!.Value.Id() == id);

    /// <summary>The words the kind list uses.</summary>
    public static string Title(this ExportKind kind) => kind switch
    {
        ExportKind.TrimCopy => "Trim (Keyframe Copy)",
        ExportKind.TrimExact => "Trim (Exact)",
        ExportKind.FollowCut => "Follow Cut",
        ExportKind.Grid2 => "2-Up",
        ExportKind.Grid4 => "4-Up",
        ExportKind.Download => "Download Only",
        ExportKind.StillCurrent => "Current Picture",
        _ => "Picture of Each Camera",
    };

    /// <summary>The Export button's title for the kind.</summary>
    public static string ButtonTitle(this ExportKind kind) => kind switch
    {
        ExportKind.TrimCopy or ExportKind.TrimExact => "Trim",
        ExportKind.FollowCut or ExportKind.Grid2 or ExportKind.Grid4 => "Export",
        ExportKind.Download => "Download",
        ExportKind.StillCurrent => "Save Picture",
        _ => "Save Pictures",
    };

    /// <summary>Whether the kind is a trim.</summary>
    public static bool IsTrim(this ExportKind kind) => kind is ExportKind.TrimCopy or ExportKind.TrimExact;

    /// <summary>Whether the kind is a follow cut or grid.</summary>
    public static bool IsEdit(this ExportKind kind) => kind is ExportKind.FollowCut or ExportKind.Grid2 or ExportKind.Grid4;

    /// <summary>Whether the kind saves pictures.</summary>
    public static bool IsStill(this ExportKind kind) => kind is ExportKind.StillCurrent or ExportKind.StillEach;

    /// <summary>The request's <c>tier</c>.</summary>
    public static string Tier(this ExportQuality quality) => quality == ExportQuality.Sub ? "sub" : "main";

    /// <summary>The quality a tier names (main when unknown).</summary>
    public static ExportQuality QualityOf(string? tier) => tier == "sub" ? ExportQuality.Sub : ExportQuality.Main;

    /// <summary>The words the quality choice uses.</summary>
    public static string Title(this ExportQuality quality) => quality == ExportQuality.Sub ? "Sub-stream" : "Main Stream";

    /// <summary>The quality choice's hint.</summary>
    public static string Detail(this ExportQuality quality) =>
        quality == ExportQuality.Sub ? "Smaller files, lower quality" : "Full quality; the files are larger";

    /// <summary>The kinds that apply, in the order the panel lists them; the others are hidden.</summary>
    public static IReadOnlyList<ExportKind> Kinds(ExportContext c)
    {
        var list = new List<ExportKind>();
        if (c.HasRange && c.SourceCameras >= 1)
        {
            list.Add(ExportKind.TrimCopy);
            list.Add(ExportKind.TrimExact);
        }
        if (c.Editable && c.CanWriteEdit)
        {
            list.Add(ExportKind.FollowCut);
            if (c.SourceCameras >= 2)
            {
                list.Add(ExportKind.Grid2);
                list.Add(ExportKind.Grid4);
            }
        }
        if (c.HasRange && c.SourceCameras >= 1)
        {
            list.Add(ExportKind.Download);
        }
        if (c.HasPictures)
        {
            list.Add(ExportKind.StillCurrent);
            if (c.SourceCameras >= 2)
            {
                list.Add(ExportKind.StillEach);
            }
        }
        return list;
    }

    /// <summary>The kind to show: the one chosen before when it still applies, else the first that does.</summary>
    public static ExportKind? DefaultKind(IReadOnlyList<ExportKind> available, ExportKind? keeping) =>
        keeping is { } k && available.Contains(k) ? k : available.Count > 0 ? available[0] : null;

    /// <summary>Whether the camera checklist applies (the follow cut and grids use the incident's files, and the current picture is one).</summary>
    public static bool UsesCameras(ExportKind kind) => kind.IsTrim() || kind is ExportKind.Download or ExportKind.StillEach;

    /// <summary>Whether the sound switch applies.</summary>
    public static bool UsesSound(ExportKind kind) => kind.IsEdit();

    /// <summary>Whether the stream choice applies: a download, and a trim that has to download its range first.</summary>
    public static bool UsesQuality(ExportKind kind, bool editable) => kind == ExportKind.Download || (kind.IsTrim() && !editable);

    /// <summary>Why an export cannot start, or null. <paramref name="start"/> and <paramref name="end"/> are the range in milliseconds.</summary>
    public static string? Problem(ExportKind kind, long start, long end, int checkedCameras, bool busy)
    {
        if (busy)
        {
            return "Wait for the export in progress, or cancel it";
        }
        if (UsesCameras(kind) && checkedCameras < 1)
        {
            return "Choose at least one camera";
        }
        if (!kind.IsStill() && end - start < MinimumMs)
        {
            return "Set an in point and an out point first (I and O)";
        }
        return null;
    }

    /// <summary>"1:05" or "1:02:05" for a duration.</summary>
    public static string Duration(long ms)
    {
        long s = Math.Max(0, ms) / 1000;
        long h = s / 3600, m = s % 3600 / 60, sec = s % 60;
        return h > 0 ? string.Create(CultureInfo.InvariantCulture, $"{h}:{m:00}:{sec:00}") : string.Create(CultureInfo.InvariantCulture, $"{m}:{sec:00}");
    }

    /// <summary>"about 25 MB" for a byte count (decimal units, like the Downloads list).</summary>
    public static string Size(long bytes)
    {
        double b = bytes;
        CultureInfo c = CultureInfo.InvariantCulture;
        if (b >= 1e9)
        {
            return "about " + (b / 1e9).ToString("F1", c) + " GB";
        }
        if (b >= 1e6)
        {
            return "about " + (b / 1e6).ToString("F0", c) + " MB";
        }
        return "about " + Math.Max(1, b / 1e3).ToString("F0", c) + " KB";
    }

    /// <summary>The live line under "What": the cameras, the duration, and the size when the core can tell.</summary>
    public static string Summary(ExportKind kind, long durationMs, int checkedCameras, long? estimatedBytes)
    {
        static string Cameras(int n) => n == 1 ? "1 camera" : $"{n} cameras";
        switch (kind)
        {
            case ExportKind.StillCurrent:
                return "1 picture";
            case ExportKind.StillEach:
                return checkedCameras == 1 ? "1 picture" : $"{checkedCameras} pictures";
            case ExportKind.FollowCut or ExportKind.Grid2 or ExportKind.Grid4:
                return $"One video, {Duration(durationMs)}";
            default:
                string s = $"{Cameras(checkedCameras)}, {Duration(durationMs)}";
                return estimatedBytes is > 0 ? s + ", " + Size(estimatedBytes.Value) : s;
        }
    }

    /// <summary>The bytes of the spans' parts inside <paramref name="start"/> to <paramref name="end"/> (device-local seconds), each span's bytes shared evenly over its length; null when no span says.</summary>
    public static long? Estimate(IEnumerable<(long Start, long End, long Bytes)> spans, long start, long end)
    {
        double total = 0;
        bool known = false;
        foreach ((long s, long e, long bytes) in spans)
        {
            if (bytes <= 0 || e <= s)
            {
                continue;
            }
            long overlap = Math.Min(e, end) - Math.Max(s, start);
            if (overlap <= 0)
            {
                continue;
            }
            known = true;
            total += bytes * (double)overlap / (e - s);
        }
        return known ? (long)Math.Round(total, MidpointRounding.AwayFromZero) : null;
    }

    /// <summary>"07:42:05", with ".250" when the milliseconds are not zero, for a device-local time.</summary>
    public static string TimeText(long ms)
    {
        long t = ((ms % DayMs) + DayMs) % DayMs;
        long h = t / 3_600_000, m = t / 60_000 % 60, s = t / 1000 % 60, f = t % 1000;
        CultureInfo c = CultureInfo.InvariantCulture;
        return f == 0 ? string.Create(c, $"{h:00}:{m:00}:{s:00}") : string.Create(c, $"{h:00}:{m:00}:{s:00}.{f:000}");
    }

    /// <summary>The device-local time that <paramref name="text"/> ("7:42", "07:42:05", "07:42:05.25") names on the day starting at <paramref name="dayStartMs"/>; null when it is not a time.</summary>
    public static long? ParseTime(string text, long dayStartMs)
    {
        string[] parts = text.Trim().Split(':');
        if (parts.Length is < 2 or > 3 || !parts.All(p => p.Length > 0 && p.All(ch => ch is >= '0' and <= '9' or '.')))
        {
            return null;
        }
        double seconds = 0;
        if (parts.Length == 3 && (!double.TryParse(parts[2], NumberStyles.Float, CultureInfo.InvariantCulture, out seconds) || seconds >= 60))
        {
            return null;
        }
        if (!int.TryParse(parts[0], NumberStyles.None, CultureInfo.InvariantCulture, out int h) || h >= 24
            || !int.TryParse(parts[1], NumberStyles.None, CultureInfo.InvariantCulture, out int m) || m >= 60)
        {
            return null;
        }
        return dayStartMs + h * 3_600_000L + m * 60_000L + (long)Math.Round(seconds * 1000, MidpointRounding.AwayFromZero);
    }
}
