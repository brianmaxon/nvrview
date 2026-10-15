// The Windows captions engine (docs/ARCHITECTURE.md "Live transcription"): Moonshine streaming
// (MIT) behind ITranscriptionEngine, one stream per source on one shared model
// (MoonshineNative.cs). Audio is queued from any thread; a thread of the session hands it to the
// stream and asks for the transcript after every 200 ms of new audio (what the library itself
// waits for between full analyses), and StreamingCaptions (NvrView.Tiles) turns the growing list
// of lines into partial and final captions. Where the model cannot be loaded, the session hands
// itself and the audio queued so far to the fallback engine (the desktop recognizer of Windows),
// so a call never loses its captions to a broken download.

using System.Collections.Concurrent;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>Moonshine streaming captions for one source, with a fallback engine when the model cannot be loaded.</summary>
internal sealed class MoonshineTranscriber(string modelFolder, uint arch, Func<ITranscriptionEngine> fallback) : ITranscriptionEngine
{
    /// <summary>Audio between two transcripts: 200 ms at 16 kHz.</summary>
    public const int SamplesPerUpdate = 3_200;

    private readonly BlockingCollection<float[]> _queue = new(boundedCapacity: 512);
    private readonly StreamingCaptions _captions = new();
    private Thread? _thread;
    private ITranscriptionEngine? _fallback;
    private int _dropped, _updates;
    private volatile bool _finished;

    public Action<string>? OnPartial { get; set; }

    public Action<string>? OnFinal { get; set; }

    public Action<TranscriptionStatus>? OnStatus { get; set; }

    /// <summary>Which engine runs this session ("moonshine", or the fallback's name once it took over), for the log and the self-test.</summary>
    public string Engine { get; private set; } = "moonshine";

    /// <summary>What happened so far, for diagnostics.</summary>
    public string Counts => $"{Engine}: {_updates} transcripts, {_captions.Finals} lines, {_dropped} blocks dropped";

    public void Start(TranscriptSource source)
    {
        OnStatus?.Invoke(TranscriptionStatus.Preparing);
        _thread = new Thread(() => Run(source)) { Name = $"captions-{source}", IsBackground = true };
        _thread.Start();
    }

    public void Feed(ReadOnlySpan<float> pcm16k)
    {
        if (_finished || pcm16k.IsEmpty)
        {
            return;
        }
        try
        {
            // Never blocks the audio thread: a session that fell this far behind (about 25 s of
            // 50 ms blocks) loses the newest audio, not the call.
            if (!_queue.TryAdd(pcm16k.ToArray()))
            {
                Interlocked.Increment(ref _dropped);
            }
        }
        catch (InvalidOperationException)
        {
            // Finished between the check and the add.
        }
    }

    public void Finish()
    {
        if (_finished)
        {
            return;
        }
        _finished = true;
        _queue.CompleteAdding();
    }

    public void Dispose()
    {
        Finish();
        if (_thread is { IsAlive: true } t && t != Thread.CurrentThread)
        {
            _ = t.Join(TimeSpan.FromSeconds(5));
        }
        _fallback?.Dispose();
    }

    private void Run(TranscriptSource source)
    {
        MoonshineNative native;
        try
        {
            native = MoonshineNative.Acquire(modelFolder, AppContext.BaseDirectory, arch);
        }
        catch (Exception e) when (e is MoonshineException or DllNotFoundException or EntryPointNotFoundException or BadImageFormatException or IOException)
        {
            AppLog.Warn($"captions: Moonshine is not available ({e.Message}); the desktop recognizer of Windows takes over");
            HandOver(source);
            return;
        }
        int stream = -1;
        try
        {
            stream = native.CreateStream();
            OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Running));
            int pending = 0;
            foreach (float[] block in _queue.GetConsumingEnumerable())
            {
                native.AddAudio(stream, block);
                pending += block.Length;
                if (pending >= SamplesPerUpdate)
                {
                    pending = 0;
                    Send(native.Transcribe(stream));
                }
            }
            // The end: what is left is analyzed and every line is complete.
            native.Stop(stream);
            Send(native.Transcribe(stream));
            OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Finished));
        }
        catch (MoonshineException e)
        {
            // Anything the engine reports ends this transcript, never the call.
            AppLog.Warn($"captions: Moonshine stopped: {e.Message}");
            OnStatus?.Invoke(new TranscriptionStatus(TranscriptionPhase.Unavailable, "the speech recognizer stopped"));
        }
        finally
        {
            if (stream >= 0)
            {
                native.FreeStream(stream);
            }
            MoonshineNative.Release();
        }
    }

    private void Send(List<StreamingCaptions.Line> lines)
    {
        _updates++;
        foreach (StreamingCaptions.Caption c in _captions.Update(lines))
        {
            if (c.Final)
            {
                OnFinal?.Invoke(c.Text);
            }
            else
            {
                OnPartial?.Invoke(c.Text);
            }
        }
    }

    // The fallback gets this session's callbacks and every block queued so far and from now on.
    private void HandOver(TranscriptSource source)
    {
        ITranscriptionEngine next = fallback();
        next.OnPartial = OnPartial;
        next.OnFinal = OnFinal;
        next.OnStatus = OnStatus;
        _fallback = next;
        Engine = next.GetType().Name;
        next.Start(source);
        foreach (float[] block in _queue.GetConsumingEnumerable())
        {
            next.Feed(block);
        }
        next.Finish();
    }
}
