// Tests of saved call transcripts (NvrView.Tiles/TranscriptFiles.cs). The vectors are the macOS
// TranscriptFileTests' (apps/macos NVRCoreTests), the same numbers in the same order; keep the two
// copies in step.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class TranscriptFileTests
{
    /// <summary>2023-11-14 22:13:20 UTC.</summary>
    private static readonly DateTime T0 = new(2023, 11, 14, 22, 13, 20, DateTimeKind.Utc);
    private static readonly TimeZoneInfo Utc = TimeZoneInfo.Utc;
    private static readonly TimeZoneInfo Minus4 = TimeZoneInfo.CreateCustomTimeZone("test-minus-4", TimeSpan.FromHours(-4), "test", "test");
    private static readonly TimeZoneInfo Plus530 = TimeZoneInfo.CreateCustomTimeZone("test-plus-530", new TimeSpan(5, 30, 0), "test", "test");

    public static IEnumerable<(string, Action)> All() =>
    [
        ("transcripts: file names carry the start and the camera", FileNames),
        ("transcripts: camera names are made safe", CameraNames),
        ("transcripts: names do not collide", UniqueNames),
        ("transcripts: the text has the header, then each line with its time and side", TextVectors),
        ("transcripts: the oldest go first beyond the bounds", Pruning),
        ("transcripts: a call gives its file only when something was said", CallFile),
    ];

    private static void FileNames()
    {
        Check.Equal("2023-11-14 22.13.20 Front door.txt", TranscriptFiles.FileName(T0, "Front door", Utc), "UTC");
        Check.Equal("2023-11-14 18.13.20 Front door.txt", TranscriptFiles.FileName(T0, "Front door", Minus4), "UTC-4");
        Check.Equal("2023-11-15 03.43.20 a-b.txt", TranscriptFiles.FileName(T0, "a/b", Plus530), "UTC+5:30");
    }

    private static void CameraNames()
    {
        (string Name, string Safe)[] vectors =
        [
            ("Front door", "Front door"),
            ("  Back/yard: gate*?  ", "Back-yard- gate--"),
            ("Porch\t\nCam", "Porch Cam"),
            ("...", "Camera"),
            ("", "Camera"),
            ("Garage.", "Garage"),
            ("a\u0001b", "a-b"),
            ("<Side> \"door\" | 2", "-Side- -door- - 2"),
            (new string('x', 70), new string('x', 60)),
        ];
        foreach ((string name, string safe) in vectors)
        {
            Check.Equal(safe, TranscriptFiles.Sanitized(name), name);
        }
    }

    private static void UniqueNames()
    {
        Check.Equal("a.txt", TranscriptFiles.UniqueName("a.txt", []), "free");
        Check.Equal("a 2.txt", TranscriptFiles.UniqueName("a.txt", ["A.TXT"]), "taken, other case");
        Check.Equal("a 3.txt", TranscriptFiles.UniqueName("a.txt", ["a.txt", "a 2.txt"]), "two taken");
    }

    private static void TextVectors()
    {
        TranscriptLine[] lines =
        [
            new(TranscriptSource.Visitor, "Hello there.", true, T0.AddSeconds(7)),
            new(TranscriptSource.You, "One moment\nplease.", true, T0.AddSeconds(12)),
            new(TranscriptSource.Visitor, "Thanks", true),
        ];
        string text = TranscriptFiles.Text("Front door", 2, T0, T0.AddSeconds(72.4), lines, Minus4);
        Check.Equal(
            "NVRView call transcript\n" +
            "Camera: Front door\n" +
            "Call: doorbell, 2 presses\n" +
            "Started: 2023-11-14 18:13:20 UTC-04:00\n" +
            "Ended: 2023-11-14 18:14:32 (72 s)\n" +
            "\n" +
            "18:13:27 Visitor: Hello there.\n" +
            "18:13:32 You: One moment please.\n" +
            "--:--:-- Visitor: Thanks\n",
            text,
            "a doorbell call");
        string conversation = TranscriptFiles.Text("Side", 0, T0, T0.AddSeconds(-5), [], Utc);
        Check.Equal(
            "NVRView call transcript\n" +
            "Camera: Side\n" +
            "Call: conversation started in the app\n" +
            "Started: 2023-11-14 22:13:20 UTC\n" +
            "Ended: 2023-11-14 22:13:15 (0 s)\n" +
            "\n",
            conversation,
            "a conversation");
        string one = TranscriptFiles.Text("Side", 1, T0, T0, [], Plus530);
        Check.True(one.Contains("Call: doorbell, 1 press\n", StringComparison.Ordinal) && one.Contains("Started: 2023-11-15 03:43:20 UTC+05:30\n", StringComparison.Ordinal), "one press, UTC+05:30");
    }

    private static StoredTranscript F(string name, long bytes, double t) => new(name, bytes, T0.AddSeconds(t));

    private static void Pruning()
    {
        StoredTranscript[] five = [F("a", 100, 1), F("b", 100, 2), F("c", 100, 3), F("d", 100, 4), F("e", 100, 5)];
        Check.Equal("a,b", string.Join(",", TranscriptFiles.ToDelete(five.Reverse(), 3, 1000)), "count bound");
        Check.Equal("a,b,c", string.Join(",", TranscriptFiles.ToDelete(five, 10, 250)), "size bound");
        Check.Equal("", string.Join(",", TranscriptFiles.ToDelete(five, 10, 1000)), "within both");
        // The newest is kept even when it alone is over the size bound.
        Check.Equal("", string.Join(",", TranscriptFiles.ToDelete([F("x", 5000, 9)], 10, 1000)), "newest kept");
        Check.Equal("y", string.Join(",", TranscriptFiles.ToDelete([F("y", 10, 1), F("x", 5000, 9)], 10, 1000)), "older goes");
        // Same time: the later name is the newer.
        Check.Equal("A", string.Join(",", TranscriptFiles.ToDelete([F("B", 1, 1), F("A", 1, 1)], 1, 1000)), "tie by name");
        // The defaults: 500 files, 50 MB.
        StoredTranscript[] many = [.. Enumerable.Range(0, 501).Select(i => F(i.ToString("000", System.Globalization.CultureInfo.InvariantCulture), 1, i))];
        Check.Equal("000", string.Join(",", TranscriptFiles.ToDelete(many)), "defaults");
        Check.True(TranscriptFiles.MaxFiles == 500 && TranscriptFiles.MaxBytes == 52_428_800, "the bounds");
    }

    private static void CallFile()
    {
        var door = new CameraRef("d-1", 3);
        var call = new Call(door, T0, 0, new CallAudioDecision(false, true, []));
        Check.True(call.TranscriptFile("Front door", T0, Utc) is null, "nothing said");
        call.Transcript.Partial("Hel", TranscriptSource.Visitor);
        Check.True(call.TranscriptFile("Front door", T0, Utc) is null, "only a partial");
        call.Transcript.Final("Hello", TranscriptSource.Visitor, T0.AddSeconds(3));
        (string Name, string Text)? file = call.TranscriptFile("Front door", T0.AddSeconds(10), Utc);
        Check.Equal("2023-11-14 22.13.20 Front door.txt", file?.Name, "name");
        Check.True(file?.Text.EndsWith("\n22:13:23 Visitor: Hello\n", StringComparison.Ordinal) == true, "the line");
        Check.True(file?.Text.Contains("Call: doorbell, 1 press\n", StringComparison.Ordinal) == true, "one press");
    }
}
