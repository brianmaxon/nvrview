// The app's AAC-LC encoder for recordings (docs/CAPTURE.md "Encoding", docs/FFI.md "Capture"):
// the core hands it 1024-sample frames of mono float PCM for each track that needs encoding,
// and it answers each AAC access unit, in order, through its Answer callback (the app passes
// ICapture.RecordAudioEncoded).
//
// Built on the system's AAC encoder MFT (CLSID_AACMFTEncoder, a synchronous transform): one per
// open track, 16-bit PCM in at the track's rate, raw AAC-LC access units out at the nearest
// bit rate the encoder offers to the one asked (96 kbps mono). Open reports the
// AudioSpecificConfig (the encoder's own, from its output type's user data) and the encoder's
// delay in frames, which is measured once per process by encoding an impulse and decoding it
// again with the system's AAC decoder (the same method as the macOS encoder's priming check), so
// it is what a player sees; the core writes it as the edit list.
//
// Threads: the core calls it on its recorders' threads, never at once for one recording;
// different recordings may call at once. Each frame is encoded on the calling thread (a frame
// takes well under a millisecond), so the units go back in order with no queue of its own; the
// core bounds what waits for the encoder. At most MaxTracks tracks are open at a time; more are
// refused. Close with flush drains the encoder.

using System.Collections.Concurrent;
using NvrView.Interop;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.CLSCTX;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.MFAudioFormat;
using static TerraFX.Interop.Windows.MFT;

namespace NvrView.Video;

/// <summary>Answers one encoded unit; returns the core's status.</summary>
public delegate NvStatus AacAnswer(ulong recording, uint track, ReadOnlySpan<byte> unit);

/// <summary>The AAC-LC encoder behind <see cref="ICaptureEncoder"/> (see the file comment).</summary>
public sealed unsafe class AacCaptureEncoder(AacAnswer answer) : ICaptureEncoder
{
    /// <summary>The most tracks open at a time.</summary>
    public const int MaxTracks = 32;

    /// <summary>Samples in a frame.</summary>
    public const int FrameSamples = 1024;

    private readonly record struct Key(ulong Recording, uint Track);

    private readonly ConcurrentDictionary<Key, AacTrack> _open = new();
    private int _refused;
    private int _answered;

    /// <summary>Units the core refused (anything but Ok), for diagnostics.</summary>
    public int Refused => Volatile.Read(ref _refused);

    /// <summary>Units answered in all.</summary>
    public int Answered => Volatile.Read(ref _answered);

    /// <summary>Tracks open now.</summary>
    public int OpenTracks => _open.Count;

    /// <summary>Why the last Open failed (for logs and reports).</summary>
    public string? LastError { get; private set; }

    /// <summary>
    /// The encoder's delay in frames, as a player sees it: measured once per process; 0 until
    /// measured, and the AAC-LC default of 1024 if the measurement failed.
    /// </summary>
    public static uint MeasuredPriming() => AacRoundTrip.Priming;

    /// <inheritdoc/>
    public EncoderOpened? Open(ulong recording, uint track, uint sampleRate, uint channels, uint bitRate)
    {
        if (channels != 1 || !AacFormat.Supported(sampleRate) || _open.Count >= MaxTracks)
        {
            LastError = $"unsupported track: {sampleRate} Hz, {channels} channels, {_open.Count} open";
            return null;
        }
        var key = new Key(recording, track);
        if (_open.ContainsKey(key))
        {
            return null;
        }
        AacTrack? t = AacTrack.Create(sampleRate, bitRate, out string? error);
        if (t is null)
        {
            LastError = error;
            return null;
        }
        _open[key] = t;
        return new EncoderOpened(t.AudioSpecificConfig, AacRoundTrip.Priming);
    }

    /// <inheritdoc/>
    public void Encode(ulong recording, uint track, ReadOnlySpan<float> pcm)
    {
        if (!_open.TryGetValue(new Key(recording, track), out AacTrack? t))
        {
            return;
        }
        t.Encode(pcm, (unit) => Answer(recording, track, unit));
    }

    /// <inheritdoc/>
    public void Close(ulong recording, uint track, bool flush)
    {
        if (!_open.TryRemove(new Key(recording, track), out AacTrack? t))
        {
            return;
        }
        if (flush)
        {
            t.Drain((unit) => Answer(recording, track, unit));
        }
        t.Dispose();
    }

    private void Answer(ulong recording, uint track, ReadOnlySpan<byte> unit)
    {
        if (answer(recording, track, unit) == NvStatus.Ok)
        {
            Interlocked.Increment(ref _answered);
        }
        else
        {
            Interlocked.Increment(ref _refused);
        }
    }
}

/// <summary>The AAC-LC facts the encoder and the check around it agree on.</summary>
internal static class AacFormat
{
    /// <summary>The rates the system encoder takes.</summary>
    public static bool Supported(uint rate) => rate is 44_100 or 48_000;

    /// <summary>The AAC-LC AudioSpecificConfig for mono at <paramref name="rate"/> (two bytes).</summary>
    public static byte[] AudioSpecificConfig(uint rate)
    {
        int index = rate == 44_100 ? 4 : 3;
        // 5 bits object type (2: AAC-LC), 4 bits frequency index, 4 bits channel configuration, 3 bits zero.
        int bits = (2 << 11) | (index << 7) | (1 << 3);
        return [(byte)(bits >> 8), (byte)bits];
    }

    /// <summary>The encoder's bytes per second nearest to a bit rate (its output type offers 12000, 16000, 20000, 24000).</summary>
    public static uint BytesPerSecond(uint bitRate)
    {
        uint[] offered = [12_000, 16_000, 20_000, 24_000];
        uint want = bitRate / 8;
        return offered.OrderBy(o => Math.Abs((long)o - want)).First();
    }
}

/// <summary>One open track: an encoder MFT and the samples it reads and writes.</summary>
internal sealed unsafe class AacTrack : IDisposable
{
    private IMFTransform* _mft;
    private IMFSample* _out;
    private IMFMediaBuffer* _outBuffer;
    private uint _outCapacity;
    private readonly uint _rate;
    private long _frames;

    private AacTrack(uint rate)
    {
        _rate = rate;
    }

    /// <summary>The two-byte (or the encoder's own) AudioSpecificConfig.</summary>
    public byte[] AudioSpecificConfig { get; private set; } = [];

    /// <summary>Creates the encoder for a track; null (and why) if the system cannot.</summary>
    public static AacTrack? Create(uint rate, uint bitRate, out string? error)
    {
        error = null;
        Com.EnsureMta();
        GpuDevice.StartMediaFoundation();
        var track = new AacTrack(rate);
        if (!track.Setup(bitRate, out error))
        {
            track.Dispose();
            return null;
        }
        return track;
    }

    private bool Setup(uint bitRate, out string? error)
    {
        error = null;
        Guid clsid = new("93AF0C51-2275-45D2-A35B-F2BA21CAED00"); // CLSID_AACMFTEncoder
        IMFTransform* mft = null;
        HRESULT hr = CoCreateInstance(&clsid, null, (uint)CLSCTX_INPROC_SERVER, __uuidof<IMFTransform>(), (void**)&mft);
        if (hr.FAILED || mft == null)
        {
            error = $"no AAC encoder (0x{hr.Value:X8})";
            return false;
        }
        _mft = mft;

        // The output type first (the encoder wants it before the input).
        IMFMediaType* output = null;
        if (MFCreateMediaType(&output).FAILED)
        {
            error = "no media type";
            return false;
        }
        Guid audio = MFMediaType_Audio;
        Guid aac = MFAudioFormat_AAC;
        _ = output->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &audio);
        _ = output->SetGUID(Ref(in MF_MT_SUBTYPE), &aac);
        _ = output->SetUINT32(Ref(in MF_MT_AUDIO_BITS_PER_SAMPLE), 16);
        _ = output->SetUINT32(Ref(in MF_MT_AUDIO_SAMPLES_PER_SECOND), _rate);
        _ = output->SetUINT32(Ref(in MF_MT_AUDIO_NUM_CHANNELS), 1);
        _ = output->SetUINT32(Ref(in MF_MT_AUDIO_AVG_BYTES_PER_SECOND), AacFormat.BytesPerSecond(bitRate));
        _ = output->SetUINT32(Ref(in MF_MT_AUDIO_BLOCK_ALIGNMENT), 1);
        _ = output->SetUINT32(Ref(in MF_MT_AAC_PAYLOAD_TYPE), 0);
        _ = output->SetUINT32(Ref(in MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION), 0x29);
        hr = mft->SetOutputType(0, output, 0);
        output->Release();
        if (hr.FAILED)
        {
            error = $"the AAC encoder refused its output type (0x{hr.Value:X8})";
            return false;
        }

        IMFMediaType* input = null;
        if (MFCreateMediaType(&input).FAILED)
        {
            error = "no media type";
            return false;
        }
        Guid pcm = MFAudioFormat_PCM;
        _ = input->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &audio);
        _ = input->SetGUID(Ref(in MF_MT_SUBTYPE), &pcm);
        _ = input->SetUINT32(Ref(in MF_MT_AUDIO_BITS_PER_SAMPLE), 16);
        _ = input->SetUINT32(Ref(in MF_MT_AUDIO_SAMPLES_PER_SECOND), _rate);
        _ = input->SetUINT32(Ref(in MF_MT_AUDIO_NUM_CHANNELS), 1);
        _ = input->SetUINT32(Ref(in MF_MT_AUDIO_BLOCK_ALIGNMENT), 2);
        _ = input->SetUINT32(Ref(in MF_MT_AUDIO_AVG_BYTES_PER_SECOND), _rate * 2);
        hr = mft->SetInputType(0, input, 0);
        input->Release();
        if (hr.FAILED)
        {
            error = $"the AAC encoder refused 16-bit PCM at {_rate} Hz (0x{hr.Value:X8})";
            return false;
        }

        AudioSpecificConfig = ReadConfig(mft) ?? AacFormat.AudioSpecificConfig(_rate);

        MFT_OUTPUT_STREAM_INFO info;
        if (mft->GetOutputStreamInfo(0, &info).FAILED)
        {
            error = "no output stream info";
            return false;
        }
        _outCapacity = Math.Max(info.cbSize, 4096u);
        IMFSample* outSample = null;
        IMFMediaBuffer* outBuffer = null;
        if (MFCreateSample(&outSample).FAILED || MFCreateMemoryBuffer(_outCapacity, &outBuffer).FAILED)
        {
            error = "no samples";
            return false;
        }
        _ = outSample->AddBuffer(outBuffer);
        _out = outSample;
        _outBuffer = outBuffer;
        _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
        _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        return true;
    }

    // The AudioSpecificConfig the encoder put in its output type's user data (after the 12 bytes of
    // HEAACWAVEINFO's extension); null if it is not there or is not AAC-LC.
    private static byte[]? ReadConfig(IMFTransform* mft)
    {
        IMFMediaType* type = null;
        if (mft->GetOutputCurrentType(0, &type).FAILED || type == null)
        {
            return null;
        }
        try
        {
            uint size = 0;
            if (type->GetBlobSize(Ref(in MF_MT_USER_DATA), &size).FAILED || size is <= 12 or > 12 + 64)
            {
                return null;
            }
            byte[] blob = new byte[size];
            uint got = 0;
            fixed (byte* p = blob)
            {
                if (type->GetBlob(Ref(in MF_MT_USER_DATA), p, size, &got).FAILED || got != size)
                {
                    return null;
                }
            }
            byte[] asc = blob[12..];
            return asc.Length >= 2 && asc[0] >> 3 == 2 ? asc : null;
        }
        finally
        {
            type->Release();
        }
    }

    /// <summary>Encodes one frame of 1024 samples; each unit that comes out goes to <paramref name="emit"/>, in order.</summary>
    public void Encode(ReadOnlySpan<float> pcm, AacUnit emit)
    {
        if (_mft == null || pcm.Length != AacCaptureEncoder.FrameSamples)
        {
            return;
        }
        // A fresh sample per frame: the transform may keep a reference to its input.
        IMFSample* sample = null;
        IMFMediaBuffer* buffer = null;
        if (MFCreateSample(&sample).FAILED)
        {
            return;
        }
        if (MFCreateMemoryBuffer(AacCaptureEncoder.FrameSamples * 2, &buffer).FAILED)
        {
            sample->Release();
            return;
        }
        byte* dst;
        uint max, cur;
        if (buffer->Lock(&dst, &max, &cur).FAILED)
        {
            buffer->Release();
            sample->Release();
            return;
        }
        var samples = new Span<short>(dst, AacCaptureEncoder.FrameSamples);
        for (int i = 0; i < samples.Length; i++)
        {
            float v = Math.Clamp(pcm[i], -1f, 1f);
            samples[i] = (short)MathF.Round(v * 32767f);
        }
        _ = buffer->Unlock();
        _ = buffer->SetCurrentLength(AacCaptureEncoder.FrameSamples * 2);
        _ = sample->AddBuffer(buffer);
        buffer->Release();
        long start = _frames * AacCaptureEncoder.FrameSamples * 10_000_000L / _rate;
        _ = sample->SetSampleTime(start);
        _ = sample->SetSampleDuration(AacCaptureEncoder.FrameSamples * 10_000_000L / _rate);
        HRESULT hr = _mft->ProcessInput(0, sample, 0);
        if (hr.Value == MF_E_NOTACCEPTING)
        {
            Collect(emit);
            hr = _mft->ProcessInput(0, sample, 0);
        }
        sample->Release();
        if (hr.FAILED)
        {
            return;
        }
        _frames++;
        Collect(emit);
    }

    /// <summary>Ends the stream and emits what the encoder still holds.</summary>
    public void Drain(AacUnit emit)
    {
        if (_mft == null)
        {
            return;
        }
        _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_COMMAND_DRAIN, 0);
        Collect(emit);
    }

    // Every unit the encoder has ready.
    private void Collect(AacUnit emit)
    {
        while (true)
        {
            _ = _outBuffer->SetCurrentLength(0);
            MFT_OUTPUT_DATA_BUFFER output = default;
            output.dwStreamID = 0;
            output.pSample = _out;
            uint status = 0;
            HRESULT hr = _mft->ProcessOutput(0, 1, &output, &status);
            if (output.pEvents != null)
            {
                output.pEvents->Release();
            }
            if (hr.FAILED)
            {
                // Needs more input, or the stream ended: nothing more now.
                return;
            }
            IMFSample* sample = output.pSample;
            if (sample == null)
            {
                return;
            }
            IMFMediaBuffer* buffer = null;
            if (sample->ConvertToContiguousBuffer(&buffer).FAILED || buffer == null)
            {
                return;
            }
            byte* data;
            uint max, len;
            if (buffer->Lock(&data, &max, &len).SUCCEEDED)
            {
                if (len > 0)
                {
                    emit(new ReadOnlySpan<byte>(data, (int)len));
                }
                _ = buffer->Unlock();
            }
            buffer->Release();
            if (sample != _out)
            {
                sample->Release();
            }
        }
    }

    /// <summary>Releases the encoder and its samples.</summary>
    public void Dispose()
    {
        if (_mft != null)
        {
            _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_COMMAND_FLUSH, 0);
            _mft->Release();
            _mft = null;
        }
        if (_outBuffer != null)
        {
            _outBuffer->Release();
            _outBuffer = null;
        }
        if (_out != null)
        {
            _out->Release();
            _out = null;
        }
    }

    // TerraFX exposes GUIDs as `ref readonly` properties over static data.
    internal static Guid* Ref(in Guid g) => (Guid*)System.Runtime.CompilerServices.Unsafe.AsPointer(ref System.Runtime.CompilerServices.Unsafe.AsRef(in g));
}

/// <summary>One unit of encoded audio.</summary>
internal delegate void AacUnit(ReadOnlySpan<byte> unit);
