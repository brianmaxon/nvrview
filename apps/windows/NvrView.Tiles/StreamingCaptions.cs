// A streaming recognizer's transcript turned into the caption contract of ITranscriptionEngine
// (Calls.cs): partial results that replace each other, and one final result per phrase.
//
// The recognizer (Moonshine streaming, apps/windows/NvrView.App/MoonshineTranscriber.cs) reports
// the whole transcript of a stream on every call: a list of lines (phrases) that only grows, in
// which only the last line may still be in progress and a finished line never changes. This
// class remembers which lines were already final and what the shown partial is, so each update
// sends only what changed. Pure, so the rules are tested without the native engine
// (NvrView.Tiles.Tests, StreamingCaptionsTests).

namespace NvrView.Tiles;

/// <summary>Diffs successive transcripts of one stream into partial and final captions.</summary>
public sealed class StreamingCaptions
{
    private readonly HashSet<ulong> _finished = [];
    private string _partial = "";

    /// <summary>One line of the recognizer's transcript: a stable id, its text so far, and whether the speaker finished it.</summary>
    public readonly record struct Line(ulong Id, string Text, bool Complete);

    /// <summary>A caption to send: a final phrase, or the phrase in progress (empty clears it).</summary>
    public readonly record struct Caption(string Text, bool Final);

    /// <summary>Lines made final so far (each once).</summary>
    public int Finals => _finished.Count;

    /// <summary>
    /// The captions for the transcript now, in order: a final for every line that finished since
    /// the last update (empty lines are dropped, as the engine reports speech it could not
    /// transcribe as ""), then the line in progress when its text changed. A partial that was
    /// shown and whose line ended without text is cleared with an empty partial.
    /// </summary>
    public IReadOnlyList<Caption> Update(IReadOnlyList<Line> lines)
    {
        var captions = new List<Caption>();
        foreach (Line line in lines)
        {
            string text = line.Text.Trim();
            if (line.Complete)
            {
                if (!_finished.Add(line.Id))
                {
                    continue;
                }
                if (text.Length > 0)
                {
                    // A final replaces the phrase in progress (Transcript.Final).
                    captions.Add(new Caption(text, true));
                    _partial = "";
                }
                else if (_partial.Length > 0)
                {
                    captions.Add(new Caption("", false));
                    _partial = "";
                }
            }
            else if (!_finished.Contains(line.Id) && text != _partial)
            {
                captions.Add(new Caption(text, false));
                _partial = text;
            }
        }
        return captions;
    }
}
