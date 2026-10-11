// What a call needs from the computer (docs/UX.md "Doorbell calls"), behind small interfaces so the
// self-test and tests can fake each of them:
//
// - the microphone: WASAPI shared-mode capture, 16 kHz mono float (the engine converts whatever
//   the device's own format), on a thread of its own, in communications mode with the system's
//   echo canceller where Windows confirms one;
// - the system voice that renders a typed reply: Windows.Media.SpeechSynthesis into a WAV in
//   memory (never played here; the camera's speaker plays it), read as mono float at the voice's
//   own rate (the core resamples what talk-back sends);
// - the speech recognizer, one session per source (the camera's tapped audio, the microphone):
//   the desktop recognizer of Windows through System.Speech (SAPI) with the dictation grammar,
//   fed 16 kHz PCM. It is local and needs no model to ship, but it is an older engine than the
//   one docs/ARCHITECTURE.md plans (Moonshine); where no recognizer is installed, or it cannot
//   start, the call goes on without a transcript and says why.

using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using System.Speech.AudioFormat;
using System.Speech.Recognition;
using NvrView.Tiles;
using TerraFX.Interop.Windows;
using Windows.Media.SpeechSynthesis;
using static TerraFX.Interop.Windows.AUDCLNT;
using static TerraFX.Interop.Windows.CLSCTX;
using static TerraFX.Interop.Windows.Windows;

namespace NvrView.App;

/// <summary>The computer's microphone for talk-back.</summary>
internal interface IMicrophone
{
    /// <summary>Starts capturing; <paramref name="onBlock"/> gets 16 kHz mono blocks on a capture thread. Null when started, else why not.</summary>
    string? Start(Action<float[]> onBlock);

    /// <summary>Whether the system cancels the speakers' echo in what the last <see cref="Start"/> captures (null before one started).</summary>
    EchoCancellation? Echo { get; }

    /// <summary>Stops capturing (nothing arrives after it returns).</summary>
    void Stop();
}

/// <summary>Asks the user to allow talk-back for a camera (the confirmation of <see cref="TalkPermission"/>).</summary>
internal interface ITalkPrompter
{
    /// <summary>True for Allow; false for Cancel (or when no window can show the question: nothing is allowed).</summary>
    Task<bool> AskAsync(TalkPrompt prompt);
}

/// <summary>The real prompt: a dialog on the main window.</summary>
internal sealed class DialogTalkPrompter : ITalkPrompter
{
    public async Task<bool> AskAsync(TalkPrompt prompt)
    {
        if (App.Instance.MainWindow?.Content?.XamlRoot is not { } root)
        {
            return false;
        }
        var dialog = new Microsoft.UI.Xaml.Controls.ContentDialog
        {
            XamlRoot = root,
            Title = prompt.Title,
            Content = new Microsoft.UI.Xaml.Controls.TextBlock { Text = prompt.Message, TextWrapping = Microsoft.UI.Xaml.TextWrapping.Wrap, MaxWidth = 420 },
            PrimaryButtonText = prompt.Allow,
            CloseButtonText = prompt.Cancel,
            DefaultButton = Microsoft.UI.Xaml.Controls.ContentDialogButton.Close,
        };
        try
        {
            return await dialog.ShowAsync() == Microsoft.UI.Xaml.Controls.ContentDialogResult.Primary;
        }
        catch (Exception e) when (e is InvalidOperationException or System.Runtime.InteropServices.COMException)
        {
            // Another dialog is open: nothing is allowed; the user presses again.
            return false;
        }
    }
}

/// <summary>Renders text with the system voice.</summary>
internal interface ISpeechRenderer
{
    /// <summary>Mono samples (-1..1) and their rate; empty when the voice could not render.</summary>
    Task<(float[] Samples, uint SampleRate)> RenderAsync(string text);
}

/// <summary>The services of a call.</summary>
internal sealed class CallServices(IMicrophone microphone, ISpeechRenderer speech, Func<ITranscriptionEngine> transcriber, ITalkPrompter? prompter = null)
{
    /// <summary>Asks the user to allow talk-back for a camera (a dialog; the self-test answers by itself).</summary>
    public ITalkPrompter Prompter { get; } = prompter ?? new DialogTalkPrompter();

    public IMicrophone Microphone { get; } = microphone;

    public ISpeechRenderer Speech { get; } = speech;

    public ITranscriptionEngine MakeTranscriber() => transcriber();

    /// <summary>The real ones: WASAPI, the system voice, the desktop recognizer.</summary>
    public static CallServices Real() =>
        new(new WasapiMicrophone(() => App.Instance.Audio?.DeviceId), new WindowsSpeech(), () => new SapiTranscriber());
}

// ---- speech synthesis ----

/// <summary>The system voice (Windows.Media.SpeechSynthesis), rendered to memory.</summary>
internal sealed class WindowsSpeech : ISpeechRenderer
{
    public async Task<(float[] Samples, uint SampleRate)> RenderAsync(string text)
    {
        byte[] wav;
        try
        {
            using var synthesizer = new SpeechSynthesizer();
            using SpeechSynthesisStream stream = await synthesizer.SynthesizeTextToStreamAsync(text);
            using Stream read = stream.AsStreamForRead();
            using var memory = new MemoryStream();
            await read.CopyToAsync(memory);
            wav = memory.ToArray();
        }
        catch (Exception e) when (e is COMException or InvalidOperationException or UnauthorizedAccessException or IOException)
        {
            AppLog.Warn($"the system voice could not render the reply: {e.GetType().Name}: {e.Message}");
            return ([], 0);
        }
        return Parse(wav);
    }

    private static (float[] Samples, uint SampleRate) Parse(byte[] wav)
    {
        if (Pcm.TryReadWav(wav, out float[] mono, out uint rate))
        {
            return (mono, rate);
        }
        AppLog.Warn($"the system voice returned audio in an unknown format ({wav.Length} bytes)");
        return ([], 0);
    }
}

// ---- microphone ----

/// <summary>
/// WASAPI shared-mode capture at 16 kHz mono float (see the file comment), in communications mode
/// with the system's echo canceller (docs/AUDIO.md "Echo cancellation"): the stream's category is
/// <c>AudioCategory_Communications</c>, which asks Windows for its voice processing (echo
/// cancellation and noise suppression, where the microphone's driver or Windows offers them); the
/// canceller's reference is set to the device that plays the camera
/// (<c>IAcousticEchoCancellationControl</c>), and the stream's effects list
/// (<c>IAudioEffectsManager</c>) says whether the canceller is there and on. All of it needs
/// Windows 11 22H2 or later; before that the capture is plain. Anything not
/// confirmed counts as no echo cancellation (<see cref="EchoCancellation.Decide"/>), and the
/// call falls back to the caption gate; capture itself goes on either way.
/// </summary>
internal sealed unsafe class WasapiMicrophone(Func<string?>? playbackDevice = null) : IMicrophone
{
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT.
    private static readonly Guid IeeeFloat = new("00000003-0000-0010-8000-00aa00389b71");

    // AUDIO_EFFECT_TYPE_ACOUSTIC_ECHO_CANCELLATION (ksmedia.h).
    private static readonly Guid AecEffect = new("6f64adbe-8211-11e2-8c70-2c27d7f001fa");

    // AUDCLNT_BUFFERFLAGS_SILENT.
    private const uint SilentFlag = 0x2;

    private Thread? _thread;
    private volatile bool _stop;
    private string? _problem;
    private string? _render;
    private Action<float[]>? _sink;
    private readonly ManualResetEventSlim _opened = new();

    public EchoCancellation? Echo { get; private set; }

    public string? Start(Action<float[]> onBlock)
    {
        Stop();
        _sink = onBlock;
        _problem = null;
        _stop = false;
        Echo = null;
        _render = playbackDevice?.Invoke();
        _opened.Reset();
        _thread = new Thread(Run) { Name = "microphone", IsBackground = true };
        _thread.Start();
        return _opened.Wait(TimeSpan.FromSeconds(5)) ? _problem : "the microphone did not open in time";
    }

    public void Stop()
    {
        _stop = true;
        if (_thread is { } t)
        {
            if (t.IsAlive && t != Thread.CurrentThread)
            {
                t.Join();
            }
            _thread = null;
        }
        _sink = null;
    }

    private void Run()
    {
        _ = CoInitializeEx(null, (uint)COINIT.COINIT_MULTITHREADED);
        IMMDeviceEnumerator* enumerator = null;
        IMMDevice* device = null;
        IAudioClient* client = null;
        IAudioCaptureClient* capture = null;
        HANDLE ready = CreateEventW(null, BOOL.FALSE, BOOL.FALSE, null);
        bool started = false;
        try
        {
            HRESULT hr = CoCreateInstance(Ref(in CLSID.CLSID_MMDeviceEnumerator), null, (uint)CLSCTX_ALL,
                __uuidof<IMMDeviceEnumerator>(), (void**)&enumerator);
            if (hr.FAILED)
            {
                Opened($"no device enumerator (0x{hr.Value:X8})");
                return;
            }
            hr = enumerator->GetDefaultAudioEndpoint(EDataFlow.eCapture, ERole.eConsole, &device);
            if (hr.FAILED)
            {
                Opened($"no microphone (0x{hr.Value:X8})");
                return;
            }
            hr = device->Activate(__uuidof<IAudioClient>(), (uint)CLSCTX_ALL, null, (void**)&client);
            if (hr.FAILED)
            {
                Opened(hr.Value == unchecked((int)0x80070005)
                    ? "Windows blocks microphone access for this app (Settings > Privacy > Microphone)"
                    : $"the microphone refused (0x{hr.Value:X8})");
                return;
            }
            // Communications mode, before Initialize; without it the capture is plain (and gated).
            // The calls that set it up and confirm the canceller are Windows 11 22H2's.
            string? communications = OperatingSystem.IsWindowsVersionAtLeast(10, 0, 22621)
                ? Communications(client)
                : "echo cancellation needs Windows 11 22H2 or later";
            WAVEFORMATEXTENSIBLE format = default;
            format.Format.wFormatTag = 0xFFFE; // WAVE_FORMAT_EXTENSIBLE
            format.Format.nChannels = 1;
            format.Format.nSamplesPerSec = 16_000;
            format.Format.wBitsPerSample = 32;
            format.Format.nBlockAlign = 4;
            format.Format.nAvgBytesPerSec = 16_000 * 4;
            format.Format.cbSize = (ushort)(sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX));
            format.Samples.wValidBitsPerSample = 32;
            format.dwChannelMask = 0x4; // SPEAKER_FRONT_CENTER
            format.SubFormat = IeeeFloat;
            uint flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
                AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            // 100 ms of buffer; the engine wakes us every period.
            hr = client->Initialize(AUDCLNT_SHAREMODE.AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, (WAVEFORMATEX*)&format, null);
            if (hr.FAILED)
            {
                Opened($"mono 16 kHz capture refused (0x{hr.Value:X8})");
                return;
            }
            if (communications is not null)
            {
                Echo = EchoCancellation.Decide(EchoProbe.Failed(communications));
            }
            else if (OperatingSystem.IsWindowsVersionAtLeast(10, 0, 22621))
            {
                Echo = EchoCancellation.Decide(Probe(enumerator, client));
            }
            _ = client->SetEventHandle(ready);
            hr = client->GetService(__uuidof<IAudioCaptureClient>(), (void**)&capture);
            if (hr.FAILED)
            {
                Opened($"no capture client (0x{hr.Value:X8})");
                return;
            }
            hr = client->Start();
            if (hr.FAILED)
            {
                Opened($"the microphone did not start (0x{hr.Value:X8})");
                return;
            }
            started = true;
            Opened(null);
            while (!_stop)
            {
                _ = WaitForSingleObject(ready, 200);
                if (!Drain(capture))
                {
                    AppLog.Limited("microphone", 30, "the microphone stopped (the device went away?)");
                    break;
                }
            }
        }
        finally
        {
            if (started)
            {
                _ = client->Stop();
            }
            if (capture != null)
            {
                capture->Release();
            }
            if (client != null)
            {
                client->Release();
            }
            if (device != null)
            {
                device->Release();
            }
            if (enumerator != null)
            {
                enumerator->Release();
            }
            _ = CloseHandle(ready);
            CoUninitialize();
            // A failure before Opened (an exception) must not leave Start waiting.
            _opened.Set();
        }
    }

    // Asks for the communications category. Null when set, else why not.
    [SupportedOSPlatform("windows10.0.22621.0")]
    private static string? Communications(IAudioClient* client)
    {
        IAudioClient2* client2 = null;
        HRESULT hr = client->QueryInterface(__uuidof<IAudioClient2>(), (void**)&client2);
        if (hr.FAILED)
        {
            return $"no communications mode on this system (0x{hr.Value:X8})";
        }
        AudioClientProperties properties = default;
        properties.cbSize = (uint)sizeof(AudioClientProperties);
        properties.bIsOffload = BOOL.FALSE;
        properties.eCategory = AUDIO_STREAM_CATEGORY.AudioCategory_Communications;
        properties.Options = AUDCLNT_STREAMOPTIONS.AUDCLNT_STREAMOPTIONS_NONE;
        hr = client2->SetClientProperties(&properties);
        client2->Release();
        return hr.SUCCEEDED ? null : $"the microphone refused communications mode (0x{hr.Value:X8})";
    }

    // After Initialize in communications mode: the canceller's reference and whether it is on.
    [SupportedOSPlatform("windows10.0.22621.0")]
    private EchoProbe Probe(IMMDeviceEnumerator* enumerator, IAudioClient* client)
    {
        string? render = _render ?? DefaultRender(enumerator, ERole.eConsole);
        bool reference = false;
        IAcousticEchoCancellationControl* aec = null;
        if (render is not null && client->GetService(__uuidof<IAcousticEchoCancellationControl>(), (void**)&aec).SUCCEEDED)
        {
            fixed (char* id = render)
            {
                reference = aec->SetEchoCancellationRenderEndpoint(id).SUCCEEDED;
            }
            aec->Release();
        }
        else if (render is not null)
        {
            // No control (a canceller that does not take one): it listens to the default
            // communications device, which may be another one.
            reference = DefaultRender(enumerator, ERole.eCommunications) == render;
        }
        return new EchoProbe(true, Effect: Effect(client), ReferenceIsPlayback: reference);
    }

    // The stream's echo canceller as the system lists it, else unknown.
    [SupportedOSPlatform("windows10.0.22621.0")]
    private static EchoEffect Effect(IAudioClient* client)
    {
        IAudioEffectsManager* manager = null;
        if (client->GetService(__uuidof<IAudioEffectsManager>(), (void**)&manager).FAILED)
        {
            return EchoEffect.Unknown;
        }
        AUDIO_EFFECT* effects = null;
        uint count = 0;
        EchoEffect found = EchoEffect.Unknown;
        if (manager->GetAudioEffects(&effects, &count).SUCCEEDED)
        {
            found = EchoEffect.Absent;
            for (uint i = 0; i < count; i++)
            {
                if (effects[i].id == AecEffect)
                {
                    found = effects[i].state == AUDIO_EFFECT_STATE.AUDIO_EFFECT_STATE_ON ? EchoEffect.On : EchoEffect.Off;
                }
            }
            CoTaskMemFree(effects);
        }
        manager->Release();
        return found;
    }

    private static string? DefaultRender(IMMDeviceEnumerator* enumerator, ERole role)
    {
        IMMDevice* device = null;
        if (enumerator->GetDefaultAudioEndpoint(EDataFlow.eRender, role, &device).FAILED)
        {
            return null;
        }
        string? result = null;
        char* id;
        if (device->GetId(&id).SUCCEEDED)
        {
            result = new string(id);
            CoTaskMemFree(id);
        }
        device->Release();
        return result;
    }

    private void Opened(string? problem)
    {
        _problem = problem;
        _opened.Set();
    }

    // Hands every waiting packet to the sink; false when the device failed.
    private bool Drain(IAudioCaptureClient* capture)
    {
        uint packet;
        while (capture->GetNextPacketSize(&packet).SUCCEEDED && packet != 0)
        {
            byte* data;
            uint frames, flags;
            ulong position, qpc;
            if (capture->GetBuffer(&data, &frames, &flags, &position, &qpc).FAILED)
            {
                return false;
            }
            var block = new float[frames];
            if ((flags & SilentFlag) == 0)
            {
                new ReadOnlySpan<float>(data, (int)frames).CopyTo(block);
            }
            _ = capture->ReleaseBuffer(frames);
            _sink?.Invoke(block);
        }
        return true;
    }

    private static Guid* Ref(in Guid g) =>
        (Guid*)System.Runtime.CompilerServices.Unsafe.AsPointer(ref System.Runtime.CompilerServices.Unsafe.AsRef(in g));
}

// ---- speech recognition ----

/// <summary>A blocking byte pipe: the core's tap thread feeds PCM in, the recognizer reads it out.</summary>
internal sealed class PcmPipe : Stream
{
    private readonly BlockingCollection<byte[]> _queue = new(boundedCapacity: 512);
    private byte[] _current = [];
    private int _offset;
    private long _read;

    /// <summary>Reads the recognizer made.</summary>
    public int Reads { get; private set; }

    /// <summary>Blocks dropped because the recognizer fell behind.</summary>
    public int Dropped { get; private set; }

    /// <summary>Queues 16-bit PCM from any thread; never blocks (the oldest data matters least when behind, so a full pipe drops the new block).</summary>
    public void Add(ReadOnlySpan<float> samples)
    {
        if (_queue.IsAddingCompleted || samples.Length == 0)
        {
            return;
        }
        var bytes = new byte[samples.Length * 2];
        Pcm.ToInt16(samples, bytes);
        try
        {
            if (!_queue.TryAdd(bytes))
            {
                Dropped++;
            }
        }
        catch (InvalidOperationException)
        {
            // Completed between the check and the add.
        }
    }

    /// <summary>No more data: a reader gets the end of the stream after what is queued.</summary>
    public void Complete() => _queue.CompleteAdding();

    // Fills the whole request unless the pipe ends: the recognizer takes a short read for the end
    // of the audio, so a block smaller than its request must not be handed over alone.
    public override int Read(byte[] buffer, int offset, int count)
    {
        Reads++;
        int total = 0;
        while (total < count)
        {
            if (_offset >= _current.Length)
            {
                try
                {
                    _current = _queue.Take();
                    _offset = 0;
                }
                catch (InvalidOperationException)
                {
                    break; // completed and empty
                }
            }
            int n = Math.Min(count - total, _current.Length - _offset);
            Buffer.BlockCopy(_current, _offset, buffer, offset + total, n);
            _offset += n;
            total += n;
        }
        _read += total;
        return total;
    }

    public override bool CanRead => true;

    public override bool CanSeek => false;

    public override bool CanWrite => false;

    // The recognizer asks for a length and a position when it wraps the stream; a live stream has
    // no end in sight, and where it is read to is only informational.
    public override long Length => long.MaxValue;

    public override long Position
    {
        get => _read;
        set
        {
            // Not seekable; a position the recognizer sets means nothing here.
        }
    }

    public override void Flush()
    {
    }

    // The recognizer asks where it is (a seek of 0 from the current position) and may "rewind" to
    // the start of a stream it has read nothing of; a live stream stays where it is.
    public override long Seek(long offset, SeekOrigin origin) => _read;

    public override void SetLength(long value) => throw new NotSupportedException();

    public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();

    protected override void Dispose(bool disposing)
    {
        if (disposing)
        {
            _queue.CompleteAdding();
        }
        base.Dispose(disposing);
    }
}

/// <summary>The desktop recognizer of Windows (System.Speech) behind <see cref="ITranscriptionEngine"/>.</summary>
internal sealed class SapiTranscriber : ITranscriptionEngine
{
    private readonly PcmPipe _pipe = new();
    private readonly ManualResetEventSlim _done = new();
    private Thread? _thread;
    private volatile bool _finished;
    private int _hypothesized, _recognized, _rejected, _detected;
    private string _ended = "running";

    /// <summary>What the recognizer reported so far, for diagnostics.</summary>
    public string Counts => $"speech detected {_detected}, hypotheses {_hypothesized}, recognized {_recognized}, rejected {_rejected}, {_pipe.Dropped} blocks dropped, {_pipe.Reads} reads, {_ended}";

    public Action<string>? OnPartial { get; set; }

    public Action<string>? OnFinal { get; set; }

    public Action<TranscriptionStatus>? OnStatus { get; set; }

    public void Start(TranscriptSource source)
    {
        OnStatus?.Invoke(TranscriptionStatus.Preparing);
        _thread = new Thread(Run) { Name = $"recognizer-{source}", IsBackground = true };
        _thread.Start();
    }

    public void Feed(ReadOnlySpan<float> pcm16k)
    {
        if (!_finished)
        {
            _pipe.Add(pcm16k);
        }
    }

    public void Finish()
    {
        if (_finished)
        {
            return;
        }
        _finished = true;
        _pipe.Complete();
    }

    public void Dispose()
    {
        Finish();
        if (_thread is { IsAlive: true } t && t != Thread.CurrentThread)
        {
            _ = t.Join(TimeSpan.FromSeconds(5));
        }
    }

    private void Run()
    {
        try
        {
            // Not the first entry blindly: English first (the dictation grammar's language is the recognizer's).
            RecognizerInfo? info = SpeechRecognitionEngine.InstalledRecognizers()
                .OrderByDescending(r => r.Culture.Name.StartsWith("en", StringComparison.OrdinalIgnoreCase))
                .FirstOrDefault();
            if (info is null)
            {
                OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Unavailable, "no speech recognizer is installed in Windows"));
                return;
            }
            using var engine = new SpeechRecognitionEngine(info);
            engine.LoadGrammar(new DictationGrammar());
            engine.SetInputToAudioStream(_pipe, new SpeechAudioFormatInfo(16_000, AudioBitsPerSample.Sixteen, AudioChannel.Mono));
            engine.SpeechHypothesized += (_, e) =>
            {
                Interlocked.Increment(ref _hypothesized);
                OnPartial?.Invoke(e.Result.Text);
            };
            engine.SpeechRecognized += (_, e) =>
            {
                Interlocked.Increment(ref _recognized);
                OnFinal?.Invoke(e.Result.Text);
            };
            // A phrase the recognizer is not confident of still has its best guess: shown as a
            // phrase in progress, which the next result replaces.
            engine.SpeechRecognitionRejected += (_, e) =>
            {
                Interlocked.Increment(ref _rejected);
                OnPartial?.Invoke(e.Result.Text);
            };
            engine.SpeechDetected += (_, _) => Interlocked.Increment(ref _detected);
            engine.RecognizeCompleted += (_, e) =>
            {
                _ended = $"completed after {e.AudioPosition.TotalSeconds:0.0} s of audio, input ended {e.InputStreamEnded}, error {e.Error?.GetType().Name}: {e.Error?.Message}";
                _done.Set();
            };
            OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Running));
            engine.RecognizeAsync(RecognizeMode.Multiple);
            // The pipe ends when Finish is called; the recognizer then completes.
            _ = _done.Wait(Timeout.Infinite);
            OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Finished));
        }
        catch (Exception e)
        {
            // Anything the recognizer throws ends the transcript, never the app: this is a thread of
            // its own, and an unhandled exception here would end the process.
            AppLog.Warn($"speech recognition could not start: {e.GetType().Name}: {e.Message}");
            OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Unavailable, $"the Windows recognizer could not start ({e.GetType().Name})"));
        }
    }
}

// ---- fakes (self-test) ----

/// <summary>The self-test's prompt: records what was asked and answers with the next queued answer (Cancel when none).</summary>
internal sealed class FakeTalkPrompter : ITalkPrompter
{
    public List<TalkPrompt> Asked { get; } = [];

    public Queue<bool> Answers { get; } = new();

    public Task<bool> AskAsync(TalkPrompt prompt)
    {
        Asked.Add(prompt);
        return Task.FromResult(Answers.Count > 0 && Answers.Dequeue());
    }
}

/// <summary>A microphone that sends what the test says.</summary>
internal sealed class FakeMicrophone : IMicrophone
{
    private Action<float[]>? _sink;

    public int Starts { get; private set; }

    public bool Running => _sink is not null;

    /// <summary>What every start reports (the self-test sets the fallback too).</summary>
    public EchoCancellation? Echo { get; set; } = EchoCancellation.System;

    public string? Start(Action<float[]> onBlock)
    {
        _sink = onBlock;
        Starts++;
        return null;
    }

    public void Stop() => _sink = null;

    /// <summary>One block of a quiet tone, as if the user spoke (on the calling thread).</summary>
    public void Speak(int samples)
    {
        var block = new float[samples];
        for (int i = 0; i < samples; i++)
        {
            block[i] = 0.1f * MathF.Sin(i * 0.05f);
        }
        _sink?.Invoke(block);
    }
}

/// <summary>A voice that records what it was asked to say and renders a short tone.</summary>
internal sealed class FakeSpeech : ISpeechRenderer
{
    public List<string> Rendered { get; } = [];

    /// <summary>The rate it renders at (a voice's own rate is not 16 kHz; the core resamples).</summary>
    public uint Rate { get; set; } = 16_000;

    public Task<(float[] Samples, uint SampleRate)> RenderAsync(string text)
    {
        Rendered.Add(text);
        var samples = new float[Rate / 2];
        for (int i = 0; i < samples.Length; i++)
        {
            samples[i] = 0.1f * MathF.Sin(i * 0.05f);
        }
        return Task.FromResult((samples, Rate));
    }
}

/// <summary>A recognizer the test drives: it reports Running at once and counts what it was fed.</summary>
internal sealed class FakeTranscriber : ITranscriptionEngine
{
    private long _fed;

    public Action<string>? OnPartial { get; set; }

    public Action<string>? OnFinal { get; set; }

    public Action<TranscriptionStatus>? OnStatus { get; set; }

    public TranscriptSource? Source { get; private set; }

    public bool Finished { get; private set; }

    /// <summary>Samples fed so far (any thread).</summary>
    public long SamplesFed => Interlocked.Read(ref _fed);

    public void Start(TranscriptSource source)
    {
        Source = source;
        OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Running));
    }

    public void Feed(ReadOnlySpan<float> pcm16k) => Interlocked.Add(ref _fed, pcm16k.Length);

    public void Finish() => Finished = true;

    public void Dispose() => Finish();

    public void Say(string partial, string final)
    {
        OnPartial?.Invoke(partial);
        OnFinal?.Invoke(final);
    }
}
