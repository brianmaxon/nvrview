// Self-test area "calls", the computer's own engines: the system voice, the recognizers (the
// desktop recognizer of Windows and Moonshine), the captions download and its fallback, and the
// microphone. Split from SelfTest.Calls.cs.

using System.IO.Compression;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    // The computer's own voice, recognizers, and microphone, which depend on what Windows has
    // installed: reported, and failing only when something claims to work and does not.
    private async Task RealCallEngines()
    {
        // parity: call-system-voice
        (float[] samples, uint rate) = await new WindowsSpeech().RenderAsync("Hello, one moment please.");
        Say($"calls: system voice rendered {samples.Length} samples at {rate} Hz ({(rate == 0 ? 0 : samples.Length / (double)rate):0.0} s)");
        Expect(rate >= 8000 && samples.Length > rate / 2, "the system voice rendered nothing usable");

        float[] pcm = samples.Length > 0 ? To16k(samples, rate) : [];
        if (pcm.Length > 0)
        {
            // parity: call-desktop-recognizer
            using var sapi = new SapiTranscriber();
            (List<string> statuses, List<string> texts, List<string> guesses) = await Recognize(sapi, pcm);
            Say($"calls: desktop recognizer: {string.Join(", ", statuses)}; heard \"{string.Join(" ", texts)}\", guesses \"{string.Join(" | ", guesses.TakeLast(3))}\"; {sapi.Counts}");
            Expect(statuses.Any(x => x.StartsWith("Unavailable", StringComparison.Ordinal)) ||
                string.Join(" ", texts).Contains("moment", StringComparison.OrdinalIgnoreCase),
                "the desktop recognizer is available but did not hear the system voice's sentence");

            // parity: call-moonshine-recognizer
            await MoonshineRecognizer(pcm);
        }

        // parity: call-caption-engines
        await CaptionEngineChoice();

        // parity: call-microphone
        if (options.MicrophoneTest)
        {
            var mic = new WasapiMicrophone(() => app.Audio?.DeviceId);
            int blocks = 0, samplesSeen = 0;
            float peak = 0;
            string? problem = mic.Start(block =>
            {
                Interlocked.Increment(ref blocks);
                Interlocked.Add(ref samplesSeen, block.Length);
                foreach (float x in block)
                {
                    peak = Math.Max(peak, Math.Abs(x));
                }
            });
            if (problem is null)
            {
                await Until(() => Volatile.Read(ref blocks) >= 5, 3);
            }
            mic.Stop();
            Say($"calls: microphone {(problem is null ? $"opened: {blocks} blocks, {samplesSeen} samples at 16 kHz, peak {peak:0.000}" : $"not available: {problem}")}");
            // What Windows reports about its echo canceller for this microphone and the output that plays.
            Say($"calls: microphone echo cancellation: {(mic.Echo is { } echo ? echo.Active ? "on" : $"off ({echo.Why})" : "not reported")}, playback device {(app.Audio?.DeviceId is null ? "none" : "known")}");
            Expect(problem is not null || mic.Echo is not null, "the microphone opened but did not report on echo cancellation");
            Expect(problem is not null || blocks > 10, "the microphone opened but delivered no audio");
        }
    }

    // 16 kHz by linear interpolation (the voice's own rate is higher).
    private static float[] To16k(float[] samples, uint rate)
    {
        float[] pcm = new float[(int)(samples.Length * 16_000L / rate)];
        for (int i = 0; i < pcm.Length; i++)
        {
            double at = i * (double)rate / 16_000;
            int j = (int)at;
            double f = at - j;
            pcm[i] = (float)(samples[j] * (1 - f) + samples[Math.Min(j + 1, samples.Length - 1)] * f);
        }
        return pcm;
    }

    // Feeds the sentence in 100 ms blocks and a second of silence, then finishes, waiting for the
    // final text (or the engine's word that it cannot run, or that it finished).
    private static async Task<(List<string> Statuses, List<string> Texts, List<string> Guesses)> Recognize(ITranscriptionEngine engine, float[] pcm)
    {
        var statuses = new List<string>();
        var texts = new List<string>();
        var guesses = new List<string>();
        engine.OnStatus = s => { lock (statuses) { statuses.Add(s.Phase + (s.Why is null ? "" : $" ({s.Why})")); } };
        engine.OnFinal = t => { lock (texts) { texts.Add(t); } };
        engine.OnPartial = t => { lock (guesses) { guesses.Add(t); } };
        engine.Start(TranscriptSource.Visitor);
        for (int at = 0; at < pcm.Length; at += 1600)
        {
            engine.Feed(pcm.AsSpan(at, Math.Min(1600, pcm.Length - at)));
        }
        engine.Feed(new float[16_000]);
        bool Ended() { lock (statuses) { return statuses.Any(s => s.StartsWith("Unavailable", StringComparison.Ordinal) || s.StartsWith("Finished", StringComparison.Ordinal)); } }
        await Until(() => Snapshot(texts).Count > 0 || Ended(), 10);
        engine.Finish();
        // The final result after the end of the audio, or nothing more where the recognizer is unavailable.
        await Until(() => Ended() || string.Join(" ", Snapshot(texts)).Contains("moment", StringComparison.OrdinalIgnoreCase), 10);
        return (Snapshot(statuses), Snapshot(texts), Snapshot(guesses));
    }

    // Moonshine on the real model, from a folder already on this computer: checks never download it.
    // NVRVIEW_CAPTION_MODEL names the folder, else %USERPROFILE%\nvrview\models\<name> (where the
    // Windows test computers keep it, outside every worker folder); without one, skipped and said why.
    private async Task MoonshineRecognizer(float[] pcm)
    {
        CaptionModelManifest? manifest = CaptionEngines.Manifest();
        string folder = Environment.GetEnvironmentVariable("NVRVIEW_CAPTION_MODEL") is { Length: > 0 } given
            ? given
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "nvrview", "models", manifest?.Name ?? "");
        if (manifest is null || !manifest.Files.All(f => File.Exists(Path.Combine(folder, f))))
        {
            Say($"calls: Moonshine recognizer skipped: no local caption model{(manifest is null ? " manifest" : $" in {folder}")} (the self-test never downloads it)");
            return;
        }
        var fallbacks = new List<FakeTranscriber>();
        using var moonshine = new MoonshineTranscriber(folder, manifest.Arch, () =>
        {
            var fake = new FakeTranscriber();
            fallbacks.Add(fake);
            return fake;
        });
        var clock = System.Diagnostics.Stopwatch.StartNew();
        (List<string> statuses, List<string> texts, List<string> guesses) = await Recognize(moonshine, pcm);
        Say($"calls: Moonshine recognizer: {string.Join(", ", statuses)}; heard \"{string.Join(" ", texts)}\", partials {guesses.Count} (last \"{guesses.LastOrDefault()}\"); {moonshine.Counts}; {clock.Elapsed.TotalSeconds:0.0} s");
        Expect(fallbacks.Count == 0, "the Moonshine recognizer could not load the local caption model and fell back");
        Expect(string.Join(" ", texts).Contains("moment", StringComparison.OrdinalIgnoreCase), "Moonshine did not hear the system voice's sentence");
        Expect(guesses.Count > 0, "Moonshine sent no phrase in progress before the final text");
    }

    // The choice of engine and the download, on a synthetic archive in a folder source (nothing
    // from the network): no model gives the desktop recognizer and starts the install; once it is
    // installed, calls get Moonshine; a model that cannot load hands the session, with its audio, to
    // the fallback.
    private async Task CaptionEngineChoice()
    {
        string root = Path.Combine(Path.GetDirectoryName(options.Log!)!, "caption-engines");
        if (Directory.Exists(root))
        {
            Directory.Delete(root, recursive: true);
        }
        string[] files = ["encoder.ort", "moonshine.dll", "onnxruntime.dll"];
        string source = Path.Combine(root, "source");
        Directory.CreateDirectory(Path.Combine(source, "models"));
        string archive = Path.Combine(source, "models", "test-model.zip");
        using (ZipArchive zip = ZipFile.Open(archive, ZipArchiveMode.Create))
        {
            foreach (string f in files)
            {
                using StreamWriter w = new(zip.CreateEntry($"test-model/{f}").Open());
                w.Write($"not a real {f}");
            }
        }
        var manifest = new CaptionModelManifest("test-model", new FileInfo(archive).Length, await CaptionModelStore.Sha256Async(archive), files, 4);
        var fallbacks = new List<FakeTranscriber>();
        var engines = new CaptionEngines(new CaptionModelStore(manifest, Path.Combine(root, "Models")), () => source, () =>
        {
            var fake = new FakeTranscriber();
            fallbacks.Add(fake);
            return fake;
        });
        using ITranscriptionEngine first = engines.Make();
        bool installed = engines.Install is { } install && await Until(() => install.IsCompleted, 30);
        using ITranscriptionEngine second = engines.Make();
        Say($"calls: caption engines: before the download {first.GetType().Name}, download finished {installed}, after it {second.GetType().Name}");
        Expect(first is FakeTranscriber && installed && second is MoonshineTranscriber, "the captions did not start with the desktop recognizer and move to Moonshine once the download was installed");
        if (second is MoonshineTranscriber m)
        {
            var statuses = new List<TranscriptionPhase>();
            m.OnStatus = s => { lock (statuses) { statuses.Add(s.Phase); } };
            m.Start(TranscriptSource.You);
            m.Feed(new float[3_200]);
            // The first fake is the session made before the download; the second is the fallback.
            bool handed = await Until(() => fallbacks.Count == 2 && fallbacks[1].SamplesFed >= 3_200, 10);
            m.Feed(new float[1_600]);
            m.Finish();
            bool all = await Until(() => fallbacks.Count == 2 && fallbacks[1].Finished && fallbacks[1].SamplesFed == 4_800, 10);
            Say($"calls: a model that cannot load: {m.Counts}, fallback fed {(fallbacks.Count == 2 ? fallbacks[1].SamplesFed : 0)} samples, statuses {string.Join(", ", Snapshot(statuses))}");
            Expect(handed && all && fallbacks[1].Source == TranscriptSource.You, "a session whose model could not load did not hand its audio to the fallback");
            Expect(Snapshot(statuses).Contains(TranscriptionPhase.Running), "the fallback's status did not reach the call");
        }
    }

    private static List<T> Snapshot<T>(List<T> list)
    {
        lock (list)
        {
            return [.. list];
        }
    }
}
