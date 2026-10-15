// Tests of the streaming transcript diff (NvrView.Tiles/StreamingCaptions.cs): the rules that turn
// a streaming recognizer's growing list of lines into partial and final captions.

using NvrView.Tiles;
using Line = NvrView.Tiles.StreamingCaptions.Line;

namespace NvrView.Tiles.Tests;

internal static class StreamingCaptionsTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("streaming captions: a phrase in progress, then its final once", InProgressThenFinal),
        ("streaming captions: an unchanged transcript sends nothing", Unchanged),
        ("streaming captions: phrases in a row, several finished in one update", InARow),
        ("streaming captions: a line finished without text clears its partial", EmptyFinal),
        ("streaming captions: text is trimmed and empty lines in progress send nothing", Trimmed),
    ];

    private static string Show(IReadOnlyList<StreamingCaptions.Caption> captions) =>
        string.Join(" | ", captions.Select(c => (c.Final ? "F:" : "P:") + c.Text));

    private static void InProgressThenFinal()
    {
        var s = new StreamingCaptions();
        Check.Equal("P:Hello", Show(s.Update([new Line(7, " Hello", false)])), "first partial");
        Check.Equal("P:Hello, is", Show(s.Update([new Line(7, "Hello, is", false)])), "growing partial");
        Check.Equal("F:Hello, is anyone home?", Show(s.Update([new Line(7, "Hello, is anyone home?", true)])), "final");
        Check.Equal("", Show(s.Update([new Line(7, "Hello, is anyone home?", true)])), "a final is sent once");
        Check.Equal(1, s.Finals, "one final");
    }

    private static void Unchanged()
    {
        var s = new StreamingCaptions();
        s.Update([new Line(1, "Package", false)]);
        Check.Equal("", Show(s.Update([new Line(1, "Package", false)])), "same partial again");
        Check.Equal("", Show(s.Update([])), "no lines");
    }

    private static void InARow()
    {
        var s = new StreamingCaptions();
        s.Update([new Line(1, "I have a", false)]);
        string sent = Show(s.Update(
        [
            new Line(1, "I have a package for you.", true),
            new Line(2, "Can you sign?", true),
            new Line(3, "It is", false),
        ]));
        Check.Equal("F:I have a package for you. | F:Can you sign? | P:It is", sent, "two finals then the new partial");
        Check.Equal("F:It is heavy.", Show(s.Update(
        [
            new Line(1, "I have a package for you.", true),
            new Line(2, "Can you sign?", true),
            new Line(3, "It is heavy.", true),
        ])), "only the new final");
    }

    private static void EmptyFinal()
    {
        var s = new StreamingCaptions();
        s.Update([new Line(4, "uh", false)]);
        Check.Equal("P:", Show(s.Update([new Line(4, "", true)])), "the partial is cleared");
        Check.Equal(1, s.Finals, "the line counts as done");
        var quiet = new StreamingCaptions();
        Check.Equal("", Show(quiet.Update([new Line(5, " ", true)])), "nothing shown, nothing to clear");
    }

    private static void Trimmed()
    {
        var s = new StreamingCaptions();
        Check.Equal("", Show(s.Update([new Line(1, "  ", false)])), "blank in progress");
        Check.Equal("P:Hi", Show(s.Update([new Line(1, " Hi ", false)])), "trimmed");
        Check.Equal("", Show(s.Update([new Line(1, "Hi  ", false)])), "the same after trimming");
    }
}
