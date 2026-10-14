// The loudness gap action and the loudness strips' height of the Recordings
// browser (docs/UX.md "Recordings", Sound). Split from RecordingsBrowser.cs.

namespace NvrView.Shell;

public sealed partial class RecordingsBrowser
{
    /// <summary>"Analyze this gap": measures one empty stretch of a lane's loudness from the downloaded video. Only a gap the core said is analyzable.</summary>
    public void AnalyzeGap(LoudnessGap gap, RecordingCamera camera)
    {
        RecordingCamera mic = LoudnessScale.Microphone(camera);
        if (gap.CanAnalyze && !Model.IsAnalyzing(mic, Day))
        {
            Model.AnalyzeSound(mic, Day, (gap.Start, gap.End));
        }
    }

    /// <summary>The height of the loudness strips (the user drags it; kept across launches).</summary>
    public double LoudnessHeight
    {
        get => RecordingsPanes.ClampLoudness(_layout.LoudnessHeight);
        set
        {
            double h = RecordingsPanes.ClampLoudness(value);
            if (h != LoudnessHeight)
            {
                _layout.LoudnessHeight = h;
            }
        }
    }

    /// <summary>Writes the strips' height to the settings file (when a drag ends).</summary>
    public void KeepLoudnessHeight() => _layout.Save();
}
