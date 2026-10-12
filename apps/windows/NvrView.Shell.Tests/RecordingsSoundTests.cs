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
