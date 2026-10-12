// Decoding AAC again with the system's AAC decoder (CLSID_CMSAACDecMFT): the encoder's delay
// ("priming") is the position an impulse comes back at, and the encoder test (a tone through the
// encoder and back; pitch and level) reads the same decoded samples. Raw access units in (with
// the AudioSpecificConfig), 16-bit mono PCM out.

using NvrView.Interop;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.CLSCTX;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.MFAudioFormat;
using static TerraFX.Interop.Windows.MFT;

namespace NvrView.Video;

/// <summary>What <see cref="AacRoundTrip.Run"/> found.</summary>
/// <param name="AudioSpecificConfig">The encoder's AudioSpecificConfig.</param>
/// <param name="Units">The access units, in order.</param>
/// <param name="Decoded">The decoded 16-bit samples (the whole of what the decoder returned).</param>
public sealed record AacRoundTripResult(byte[] AudioSpecificConfig, IReadOnlyList<byte[]> Units, short[] Decoded);

/// <summary>Encodes audio with the app's encoder and decodes it with the system's (see the file comment).</summary>
public static unsafe class AacRoundTrip
{
    private const int ImpulseAt = 4096;
    private const int ImpulseSignal = 16 * AacCaptureEncoder.FrameSamples;

    private static readonly Lazy<uint> s_priming = new(MeasurePriming);

    /// <summary>The encoder's delay in frames at 48 kHz, as a player sees it; the AAC-LC default of 1024 when it cannot be measured.</summary>
    public static uint Priming => s_priming.Value;

    /// <summary>The measurement itself, again (the self-test reports it beside <see cref="Priming"/>): where an impulse at sample 4096 comes back, minus 4096; null if it cannot be measured.</summary>
    public static int? MeasureImpulse()
    {
        var signal = new float[ImpulseSignal];
        signal[ImpulseAt] = 0.9f;
        AacRoundTripResult? r = Run(signal, 48_000, flush: true);
        if (r is null)
        {
            return null;
        }
        int peak = -1;
        int best = 0;
        for (int i = 0; i < r.Decoded.Length; i++)
        {
            int a = Math.Abs((int)r.Decoded[i]);
            if (a > best)
            {
                best = a;
                peak = i;
            }
        }
        return best > 2000 && peak >= ImpulseAt ? peak - ImpulseAt : null;
    }

    private static uint MeasurePriming() => MeasureImpulse() is { } p ? (uint)p : 1024;

    /// <summary>
    /// Encodes <paramref name="pcm"/> (mono, a whole number of 1024-sample frames) at <paramref name="rate"/> and decodes the
    /// units again; null if either step is not possible on this computer.
    /// </summary>
    public static AacRoundTripResult? Run(ReadOnlySpan<float> pcm, uint rate, bool flush)
    {
        AacTrack? track = AacTrack.Create(rate, 96_000, out _);
        if (track is null)
        {
            return null;
        }
        var units = new List<byte[]>();
        try
        {
            for (int at = 0; at + AacCaptureEncoder.FrameSamples <= pcm.Length; at += AacCaptureEncoder.FrameSamples)
            {
                track.Encode(pcm.Slice(at, AacCaptureEncoder.FrameSamples), u => units.Add(u.ToArray()));
            }
            if (flush)
            {
                track.Drain(u => units.Add(u.ToArray()));
            }
            short[]? decoded = Decode(track.AudioSpecificConfig, rate, units);
            return decoded is null ? null : new AacRoundTripResult(track.AudioSpecificConfig, units, decoded);
        }
        finally
        {
            track.Dispose();
        }
    }

    /// <summary>Decodes raw AAC-LC access units (mono) with the system's decoder; null if it cannot.</summary>
    public static short[]? Decode(byte[] audioSpecificConfig, uint rate, IReadOnlyList<byte[]> units)
    {
        Com.EnsureMta();
        GpuDevice.StartMediaFoundation();
        Guid clsid = new("32D186A7-218F-4C75-8876-DD77273A8999"); // CLSID_CMSAACDecMFT
        IMFTransform* mft = null;
        if (CoCreateInstance(&clsid, null, (uint)CLSCTX_INPROC_SERVER, __uuidof<IMFTransform>(), (void**)&mft).FAILED || mft == null)
        {
            return null;
        }
        IMFSample* output = null;
        IMFMediaBuffer* outputBuffer = null;
        try
        {
            if (!SetTypes(mft, audioSpecificConfig, rate))
            {
                return null;
            }
            MFT_OUTPUT_STREAM_INFO info;
            if (mft->GetOutputStreamInfo(0, &info).FAILED)
            {
                return null;
            }
            bool provides = (info.dwFlags & (uint)(_MFT_OUTPUT_STREAM_INFO_FLAGS.MFT_OUTPUT_STREAM_PROVIDES_SAMPLES |
                _MFT_OUTPUT_STREAM_INFO_FLAGS.MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES)) != 0;
            if (!provides)
            {
                if (MFCreateSample(&output).FAILED || MFCreateMemoryBuffer(Math.Max(info.cbSize, 32_768u), &outputBuffer).FAILED)
                {
                    return null;
                }
                _ = output->AddBuffer(outputBuffer);
            }
            _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
            var pcm = new List<short>();
            long time = 0;
            foreach (byte[] unit in units)
            {
                IMFSample* input = MakeInput(unit, time);
                if (input == null)
                {
                    return null;
                }
                time += AacCaptureEncoder.FrameSamples * 10_000_000L / rate;
                HRESULT hr = mft->ProcessInput(0, input, 0);
                if (hr.Value == MF_E_NOTACCEPTING)
                {
                    Collect(mft, output, outputBuffer, provides, pcm);
                    hr = mft->ProcessInput(0, input, 0);
                }
                input->Release();
                if (hr.FAILED)
                {
                    return null;
                }
                Collect(mft, output, outputBuffer, provides, pcm);
            }
            _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_COMMAND_DRAIN, 0);
            Collect(mft, output, outputBuffer, provides, pcm);
            return [.. pcm];
        }
        finally
        {
            if (outputBuffer != null)
            {
                outputBuffer->Release();
            }
            if (output != null)
            {
                output->Release();
            }
            mft->Release();
        }
    }

    private static bool SetTypes(IMFTransform* mft, byte[] asc, uint rate)
    {
        IMFMediaType* input = null;
        if (MFCreateMediaType(&input).FAILED)
        {
            return false;
        }
        Guid audio = MFMediaType_Audio;
        Guid aac = MFAudioFormat_AAC;
        _ = input->SetGUID(AacTrack.Ref(in MF_MT_MAJOR_TYPE), &audio);
        _ = input->SetGUID(AacTrack.Ref(in MF_MT_SUBTYPE), &aac);
        _ = input->SetUINT32(AacTrack.Ref(in MF_MT_AUDIO_BITS_PER_SAMPLE), 16);
        _ = input->SetUINT32(AacTrack.Ref(in MF_MT_AUDIO_SAMPLES_PER_SECOND), rate);
        _ = input->SetUINT32(AacTrack.Ref(in MF_MT_AUDIO_NUM_CHANNELS), 1);
        _ = input->SetUINT32(AacTrack.Ref(in MF_MT_AUDIO_AVG_BYTES_PER_SECOND), 12_000);
        _ = input->SetUINT32(AacTrack.Ref(in MF_MT_AUDIO_BLOCK_ALIGNMENT), 1);
        _ = input->SetUINT32(AacTrack.Ref(in MF_MT_AAC_PAYLOAD_TYPE), 0);
        _ = input->SetUINT32(AacTrack.Ref(in MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION), 0x29);
        // HEAACWAVEINFO's extension (payload type 0, level 0x29, the rest zero), then the AudioSpecificConfig.
        byte[] user = new byte[12 + asc.Length];
        user[2] = 0x29;
        asc.CopyTo(user, 12);
        fixed (byte* p = user)
        {
            _ = input->SetBlob(AacTrack.Ref(in MF_MT_USER_DATA), p, (uint)user.Length);
        }
        HRESULT hr = mft->SetInputType(0, input, 0);
        input->Release();
        if (hr.FAILED)
        {
            return false;
        }
        return ChooseOutput(mft);
    }

    // 16-bit PCM out.
    private static bool ChooseOutput(IMFTransform* mft)
    {
        for (uint i = 0; ; i++)
        {
            IMFMediaType* type = null;
            if (mft->GetOutputAvailableType(0, i, &type).FAILED || type == null)
            {
                return false;
            }
            Guid subtype;
            uint bits = 0;
            bool pcm = type->GetGUID(AacTrack.Ref(in MF_MT_SUBTYPE), &subtype).SUCCEEDED && subtype == MFAudioFormat_PCM &&
                type->GetUINT32(AacTrack.Ref(in MF_MT_AUDIO_BITS_PER_SAMPLE), &bits).SUCCEEDED && bits == 16;
            HRESULT hr = pcm ? mft->SetOutputType(0, type, 0) : new HRESULT(-1);
            type->Release();
            if (pcm && hr.SUCCEEDED)
            {
                return true;
            }
        }
    }

    private static IMFSample* MakeInput(byte[] unit, long time)
    {
        IMFSample* sample = null;
        IMFMediaBuffer* buffer = null;
        if (MFCreateSample(&sample).FAILED)
        {
            return null;
        }
        if (MFCreateMemoryBuffer((uint)unit.Length, &buffer).FAILED)
        {
            sample->Release();
            return null;
        }
        byte* dst;
        uint max, cur;
        if (buffer->Lock(&dst, &max, &cur).FAILED)
        {
            buffer->Release();
            sample->Release();
            return null;
        }
        unit.CopyTo(new Span<byte>(dst, unit.Length));
        _ = buffer->Unlock();
        _ = buffer->SetCurrentLength((uint)unit.Length);
        _ = sample->AddBuffer(buffer);
        buffer->Release();
        _ = sample->SetSampleTime(time);
        return sample;
    }

    private static void Collect(IMFTransform* mft, IMFSample* own, IMFMediaBuffer* ownBuffer, bool provides, List<short> pcm)
    {
        while (true)
        {
            if (!provides)
            {
                _ = ownBuffer->SetCurrentLength(0);
            }
            MFT_OUTPUT_DATA_BUFFER output = default;
            output.dwStreamID = 0;
            output.pSample = provides ? null : own;
            uint status = 0;
            HRESULT hr = mft->ProcessOutput(0, 1, &output, &status);
            if (output.pEvents != null)
            {
                output.pEvents->Release();
            }
            if (hr.Value == MF_E_TRANSFORM_STREAM_CHANGE)
            {
                if (provides && output.pSample != null)
                {
                    output.pSample->Release();
                }
                if (!ChooseOutput(mft))
                {
                    return;
                }
                continue;
            }
            if (hr.FAILED || output.pSample == null)
            {
                return;
            }
            IMFMediaBuffer* buffer = null;
            if (output.pSample->ConvertToContiguousBuffer(&buffer).SUCCEEDED && buffer != null)
            {
                byte* data;
                uint max, len;
                if (buffer->Lock(&data, &max, &len).SUCCEEDED)
                {
                    var samples = new ReadOnlySpan<short>(data, (int)(len / 2));
                    foreach (short s in samples)
                    {
                        pcm.Add(s);
                    }
                    _ = buffer->Unlock();
                }
                buffer->Release();
            }
            if (provides)
            {
                output.pSample->Release();
            }
        }
    }
}
