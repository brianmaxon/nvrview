// Sound in the Recordings window (RecordingsSound.cs); the same vectors as
// the macOS app's RecordingsSoundTests.

namespace NvrView.Shell.Tests;

internal static class RecordingsSoundTests
{
    private const string D = "d-example";

    public static void Run()
    {
        T.Run("sound: kinds", () =>
        {
            T.Check(SoundKinds.IsSound("dog_bark", null));
            T.Check(SoundKinds.IsSound("cough", "local_audio"));
            T.Check(!SoundKinds.IsSound("people", "device"));
            T.Equal("Glass breaking", SoundKinds.Name("glass_break"));
            T.Equal("Smoke alarm", SoundKinds.Name("smoke_alarm"));
            T.Equal("Loud cough", SoundKinds.Name("loud_cough"));
            // Sounds is a filter of its own; alarms belong to it.
            T.Check(KindFilters.Of("smoke_alarm") == KindFilter.Sound && KindFilters.Of("dog_bark") == KindFilter.Sound);
            T.Check(KindFilters.Of("dog_cat") == KindFilter.Animal && KindFilters.Of("people") == KindFilter.People);
            T.Check(KindFilters.Passes(["siren"], new HashSet<KindFilter> { KindFilter.Sound }));
            T.Check(!KindFilters.Passes(["people"], new HashSet<KindFilter> { KindFilter.Sound }));
        });

        T.Run("sound: the Sounds filter matches any event from sound", () =>
        {
            Incident incident = T.NotNull(IncidentAnswer.Parse("""
                {"revision":1,"incident":{"key":"k","start":100,"end":110,"kinds":["cough"],"cameras":[],
                 "events":[{"camera":{"device":"d-example","channel":0},"kind":"cough","source":"local_audio","start":100,"end":110}]}}
                """)).Incident;
            T.Check(KindFilters.Passes(incident, new HashSet<KindFilter> { KindFilter.Sound }));
            T.Check(!KindFilters.Passes(incident, new HashSet<KindFilter> { KindFilter.People }));
            T.Check(SoundKinds.Names(incident.Events).SequenceEqual(["Cough"]));
        });

        T.Run("sound: loudness", () =>
        {
            T.Equal(300L, LoudnessScale.Bucket(86_400, 720));
            T.Equal(5L, LoudnessScale.Bucket(1800, 720));
            T.Equal(3600L, LoudnessScale.Bucket(86_400, 1));
            T.Check(LoudnessScale.Level(-80) == 0 && LoudnessScale.Level(0) == 1 && LoudnessScale.Level(-40) == 0.5);
            T.Check(LoudnessScale.Level(-100) == 0 && LoudnessScale.Level(5) == 1);
            var a = new RecordingCamera(D, 3);
            var b = new RecordingCamera(D, 3, 1);
            var c = new RecordingCamera(D, 4);
            T.Check(LoudnessScale.Microphones([a, b, c]).SequenceEqual([a, c]));
            T.Equal(a, LoudnessScale.Microphone(b));
            T.Equal("""{"start":0,"end":86399,"bucket_s":60,"cameras":[{"device":"d-example","channel":3,"lens":0}]}""",
                LoudnessScale.Query(0, 86_399, 60, [a]));
            LoudnessAnswer answer = T.NotNull(LoudnessAnswer.Parse("""
                {"revision":41,"lanes":[{"camera":{"device":"d-example","channel":3,"lens":0},"bucket_s":60,"start":0,
                 "peak_db":[-48,null,-12],"rms_db":[-60,null,-30]}]}
                """));
            T.Check(answer.Revision == 41 && answer.Lanes.Count == 1);
            T.Check(answer.Lanes[0].Peak.SequenceEqual([-48, null, -12]) && answer.Lanes[0].Rms.SequenceEqual([-60, null, -30]));
            T.Check(answer.Lanes[0].Bucket == 60 && answer.Lanes[0].Heard);
            T.Check(!new LoudnessLane(a, 60, 0, [null], [null]).Heard);
            T.Check(answer.Lanes[0].GapList.Count == 0);
            T.Equal("""{"start":0,"end":59,"bucket_s":60,"cameras":[{"device":"d-example","channel":3,"lens":0}],"gaps":true}""",
                LoudnessScale.Query(0, 59, 60, [a], gaps: true));
        });

        T.Run("sound: loudness gaps", () =>
        {
            LoudnessAnswer answer = T.NotNull(LoudnessAnswer.Parse("""
                {"revision":1,"lanes":[{"camera":{"device":"d-example","channel":3,"lens":0},"bucket_s":60,"start":0,
                 "peak_db":[null,null,-12],"rms_db":[null,null,-30],
                 "gaps":[{"start":0,"end":59,"reason":"not_recorded","analyzable":false},
                         {"start":60,"end":119,"reason":"not_analyzed","analyzable":true}]}]}
                """));
            LoudnessLane lane = answer.Lanes[0];
            T.Equal(2, lane.GapList.Count);
            T.Check(lane.GapAt(59)?.Reason == "not_recorded" && lane.GapAt(60)?.Reason == "not_analyzed");
            T.Check(lane.GapAt(120) is null);
            // The words on a gap, and which ones offer "Analyze this gap".
            T.Equal("Not recorded", new LoudnessGap(0, 1, "not_recorded").Label);
            T.Equal("No audio", new LoudnessGap(0, 1, "no_audio").Label);
            T.Equal("Not analyzed yet", new LoudnessGap(0, 1, "not_analyzed").Label);
            T.Equal("No sound analyzed", new LoudnessGap(0, 1, "other").Label);
            T.Check(lane.GapList[1].CanAnalyze && !lane.GapList[0].CanAnalyze);
            T.Check(!new LoudnessGap(0, 1, "not_analyzed", false).CanAnalyze);
            T.Check(!new LoudnessGap(0, 1, "no_audio", true).CanAnalyze);
            T.Check(lane.GapList[1].Help.Contains("Analyze this gap", StringComparison.Ordinal));
            T.Check(new LoudnessGap(0, 1, "not_analyzed").Help.Contains("download", StringComparison.Ordinal));
            var cam = new RecordingCamera(D, 3);
            T.Equal("""{"camera":{"device":"d-example","channel":3,"lens":0},"day":86400,"start":86460,"end":86519}""",
                SoundAnalysis.Query(cam, 86_400, 86_460, 86_519));
        });

        T.Run("sound: analyzing a day", () =>
        {
            var cam = new RecordingCamera(D, 3);
            T.Equal("""{"camera":{"device":"d-example","channel":3,"lens":0},"day":86400}""", SoundAnalysis.Query(cam, 86_400));
            // The words for each way it fails.
            T.Equal("Nothing downloaded for this day — download the day or an incident first", SoundAnalysis.Failure("no_audio"));
            T.Equal("Set a downloads folder in Settings", SoundAnalysis.Failure("no_source"));
            T.Equal("Could not analyze the sound (index)", SoundAnalysis.Failure("index"));
            T.Equal("Could not analyze the sound", SoundAnalysis.Failure(null));
            T.Equal("Set a downloads folder in Settings", SoundAnalysis.Refusal(NvrView.Interop.NvStatus.NotConfigured));
            T.Check(SoundAnalysis.Refusal(NvrView.Interop.NvStatus.Limit).StartsWith("Too many", StringComparison.Ordinal));
            // Events fold into an entry; a late queued or running never undoes a finished state.
            AnalysisEntry e = SoundAnalysis.Fold(null, 4, AnalysisState.Queued, null, null, null);
            T.Check(e.Active && SoundAnalysis.Progress(e) == "Waiting to analyze sound" && SoundAnalysis.Outcome(e) is null);
            e = SoundAnalysis.Fold(e, 4, AnalysisState.Running, null, null, null);
            T.Equal("Analyzing sound", SoundAnalysis.Progress(e));
            AnalysisEntry done = SoundAnalysis.Fold(e, 4, AnalysisState.Done, null, 5_400, 2);
            T.Check(!done.Active && SoundAnalysis.Progress(done) is null);
            T.Equal("Analyzed 1 h 30 min of sound; found 2 sounds", SoundAnalysis.Outcome(done));
            T.Equal(done, SoundAnalysis.Fold(done, 4, AnalysisState.Running, null, null, null));
            // A new request replaces a finished one.
            T.Check(SoundAnalysis.Fold(done, 5, AnalysisState.Queued, null, null, null).Active);
            T.Equal("No sound found in the downloaded video", SoundAnalysis.Outcome(new AnalysisEntry(4, AnalysisState.Done, null, 0, 0)));
            T.Equal("Analyzed 45 s of sound; found 1 sound", SoundAnalysis.Outcome(new AnalysisEntry(4, AnalysisState.Done, null, 45, 1)));
            T.Equal("Sound analysis canceled", SoundAnalysis.Outcome(new AnalysisEntry(4, AnalysisState.Canceled)));
            T.Equal(SoundAnalysis.Failure("no_audio"), SoundAnalysis.Outcome(new AnalysisEntry(4, AnalysisState.Failed, "no_audio")));
            T.Equal("59 s", SoundAnalysis.Duration(59));
            T.Equal("12 min", SoundAnalysis.Duration(720));
            T.Equal("2 h", SoundAnalysis.Duration(7_200));
        });

        T.Run("sound: listening", () =>
        {
            RecordingCamera[] four = [.. Enumerable.Range(0, 4).Select(i => new RecordingCamera(D, i))];
            T.Check(!SoundListening.CanAdd(new RecordingCamera(D, 9), four));
            T.Check(SoundListening.CanAdd(four[0], four));
            T.Check(SoundListening.CanAdd(new RecordingCamera("d-other", 0), four));
            T.Equal("No sound from this camera", SoundListening.Describe("refused", "no_audio"));
            T.Equal("Listening", SoundListening.Describe("watching", null));
        });
    }
}
