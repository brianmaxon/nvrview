// Tests of the caption echo filter (NvrView.Tiles/CaptionEcho.cs). The vectors are mirrored in
// apps/macos/Tests/NVRCoreTests/CaptionEchoTests.swift.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class CaptionEchoTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("echo: a garbled echo of a spoken phrase is echo", Garbled),
        ("echo: a fragment of a typed reply is echo", Fragment),
        ("echo: unrelated visitor speech is never echo", Unrelated),
        ("echo: nothing said means nothing is echo", NothingSaid),
        ("echo: outside the window is not echo", OutsideWindow),
        ("echo: the window runs from the end of talk", TalkWindow),
        ("echo: number words match digits", NumberWords),
        ("echo: a phrase in progress counts and the final replaces it", InProgress),
        ("echo: phrases said in a row can come back as one caption", InARow),
        ("echo: filter counts drops", Counts),
        ("echo: a call drops echo captions and does not count them as activity", InACall),
    ];

    private static CaptionEcho Said(string text, double at = 10, bool final = true)
    {
        var e = new CaptionEcho();
        e.Said(text, at, final);
        return e;
    }

    private static void Garbled() =>
        Check.True(Said("Hello, this is a test.").IsEcho("Hello, this is the test.", 11), "one word different");

    private static void Fragment()
    {
        CaptionEcho e = Said("Hello this is a text to speech test");
        Check.True(e.IsEcho("Hello, this is a text.", 11.5), "the start of it");
        Check.True(e.IsEcho("Hello, this is", 11), "an incomplete fragment");
    }

    private static void Unrelated()
    {
        CaptionEcho e = Said("Hello, this is a test.");
        Check.True(!e.IsEcho("Who is it? I have a package for you.", 11), "different words");
        Check.True(!e.IsEcho("Yes this is the delivery", 11), "two shared words are not enough");
        Check.True(!e.IsEcho("Hello", 11), "one shared word is not enough");
    }

    private static void NothingSaid() =>
        Check.True(!new CaptionEcho().IsEcho("Hello, this is a test.", 1), "no talk-back, no echo");

    private static void OutsideWindow()
    {
        CaptionEcho e = Said("Hello, this is a test.");
        Check.True(e.IsEcho("Hello, this is a test.", 10 + CaptionEcho.Window - 0.1), "inside");
        Check.True(!e.IsEcho("Hello, this is a test.", 10 + CaptionEcho.Window + 0.1), "outside");
    }

    private static void TalkWindow()
    {
        CaptionEcho e = Said("Hello, this is a test.");
        e.Talking(true, 10);
        Check.True(e.IsEcho("Hello this is a test", 30), "open while talking");
        e.Talking(false, 20);
        double closes = 20 + CaptionEcho.Window;
        Check.True(e.IsEcho("Hello this is a test", closes - 0.1), "open through the window");
        Check.True(!e.IsEcho("Hello this is a test", closes + 0.1), "closed after");
    }

    private static void NumberWords()
    {
        Check.True(Said("one two three four five six seven").IsEcho("1 2 3 4 5 6 7", 11), "digits for words");
        Check.True(Said("call 21 now").IsEcho("call twenty one now", 11), "two-digit numbers");
    }

    private static void InProgress()
    {
        var e = new CaptionEcho();
        e.Said("hello this is", 10, final: false);
        Check.True(e.IsEcho("hello this is", 11), "the volatile phrase counts");
        e.Said("Goodbye for now", 12, final: true);
        Check.True(!e.IsEcho("hello this is", 13), "the final replaced the volatile phrase");
        Check.True(e.IsEcho("goodbye for now", 13), "the final counts");
    }

    private static void InARow()
    {
        var e = new CaptionEcho();
        e.Said("Hello there", 10);
        e.Said("how can I help", 11);
        Check.True(e.IsEcho("hello there how can I help", 12), "joined");
    }

    private static void Counts()
    {
        CaptionEcho e = Said("Hello, this is a test.");
        Check.True(e.Filter("hello this is a test", 11), "dropped");
        Check.True(!e.Filter("who is there", 11), "kept");
        Check.Equal(1, e.Dropped, "one drop counted");
    }

    private static void InACall()
    {
        var call = new Call(new CameraRef("dev-example", 3), new DateTime(2026, 10, 10, 14, 0, 0), 0, new CallAudioDecision(false, true, []));
        call.ReplySpoken("Hello, this is a test.", 10);
        Check.True(call.Caption("Who is it", TranscriptSource.Visitor, final: true, now: 11), "unrelated speech is kept");
        Check.Equal(11.0, call.LastActivity, "and is activity");
        call.Caption("Hello, this is", TranscriptSource.Visitor, final: false, now: 12);
        Check.True(!call.Transcript.Lines.Any(l => !l.IsFinal), "the echo's volatile caption is not shown");
        Check.True(!call.Caption("Hello, this is the test.", TranscriptSource.Visitor, final: true, now: 13), "echo is dropped");
        Check.Equal(11.0, call.LastActivity, "echo is not activity");
        Check.Equal("Who is it", string.Join("|", call.Transcript.Finals.Select(l => l.Text)), "only the visitor's own speech is saved");
        Check.Equal(2, call.Echo.Dropped, "the volatile and the final caption were dropped");
        Check.True(call.Caption("Hello, this is the test.", TranscriptSource.Visitor, final: true, now: 14), "past the window the same words are the visitor's");
        Check.Equal(2, call.Transcript.Finals.Count, "kept");
    }
}
