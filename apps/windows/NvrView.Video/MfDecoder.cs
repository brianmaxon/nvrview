// One Media Foundation decoder: the platform side of Configure, Decode,
// Flush, and Reset for one tile generation (docs/RENDERING.md, docs/FFI.md).
// It makes no decisions: gating, recovery, and the color matrix are the
// core's. It decodes synchronously on the caller's thread (the core's
// per-tile delivery thread), so a Decode is answered before the action
// returns unless the decoder holds the picture back.
//
// Decoder choice: the system's synchronous H.264 or H.265 decoder MFT
// (MFTEnumEx, best merit first; H.265 comes from the HEVC Video Extension).
// Given the shared device's DXGI device manager it decodes on the GPU
// (DXVA) into D3D11 NV12 surfaces, which are copied on the GPU into the
// tile's picture. Without one (WARP, no video engine, or software forced)
// it decodes on the CPU into memory, which is uploaded.

using System.Runtime.CompilerServices;
using NvrView.Interop;
using TerraFX.Interop.DirectX;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.MFT;
using static TerraFX.Interop.Windows.MFVideoFormat;
using static TerraFX.Interop.Windows.S;

namespace NvrView.Video;

/// <summary>Where a decoder's pictures were decoded, as far as can be observed.</summary>
public enum DecoderKind
{
    /// <summary>Not known until the first picture.</summary>
    Unknown,

    /// <summary>On the GPU: output surfaces are D3D11 decoder surfaces (DXVA).</summary>
    Hardware,

    /// <summary>D3D11 output that is not a decoder surface (a software decoder writing textures).</summary>
    SoftwareTexture,

    /// <summary>On the CPU, into memory.</summary>
    Software,
}

/// <summary>Receives a decoder's answers (docs/FFI.md "Actions": exactly one per Decode not discarded).</summary>
public interface IDecodeResults
{
    /// <summary>The picture with this timestamp is in the tile's picture.</summary>
    void Decoded(long ptsUs);

    /// <summary>The oldest picture in flight (or a Configure) failed.</summary>
    void Failed(NvDecodeFailure kind);
}

/// <summary>Counters for one decoder.</summary>
public struct DecoderCounters
{
    /// <summary>Access units submitted.</summary>
    public long Submitted;

    /// <summary>Pictures that came out.</summary>
    public long Pictures;

    /// <summary>Pictures answered with NoPicture because the decoder held more than the core allows in flight.</summary>
    public long HeldTooLong;

    /// <summary>Output pictures whose timestamp matched nothing in flight.</summary>
    public long Unmatched;

    /// <summary>Input samples created (only while the pool fills, or when the decoder keeps one).</summary>
    public long InputAllocations;
}

/// <summary>
/// Process-wide decoder settings that trade memory for robustness; set them
/// before the first Configure. Measured in docs/RENDERING.md, "Windows".
/// </summary>
public static class DecoderSettings
{
    /// <summary>
    /// When positive, asks GPU decoders for this many output samples beyond
    /// what decoding needs (MF_SA_MINIMUM_OUTPUT_SAMPLE_COUNT on the output
    /// stream); 0 leaves the decoder's default.
    /// </summary>
    public static int OutputSamples { get; set; }

    /// <summary>
    /// When true, tells decoders the largest coded size to expect (the
    /// Configure's coded size, CODECAPI_AVDecVideoMaxCodedWidth/Height), so
    /// they need not size their surfaces for the largest the profile allows.
    /// </summary>
    public static bool LimitCodedSize { get; set; }
}

/// <summary>A Media Foundation decoder for one tile generation (see the file comment).</summary>
public sealed unsafe class MfDecoder : IDisposable
{
    /// <summary>
    /// Pictures the decoder may hold without output before the oldest is
    /// answered NoPicture: the core keeps at most 4 in flight (its pipeline
    /// default) and stops sending until one is answered.
    /// </summary>
    public const int MaxHeld = 3;

    private const int InputSlots = 4;

    private readonly GpuDevice _gpu;
    private readonly TilePicture _picture;
    private readonly bool _allowHardware;

    private IMFTransform* _mft;
    private bool _d3d;
    private bool _providesSamples;
    private uint _outWidth;
    private uint _outHeight;
    private int _defaultStride;
    private IMFSample* _outSample;
    private IMFMediaBuffer* _outBuffer;
    private uint _outSampleSize;

    // Input samples reused round-robin; a slot the decoder still holds is
    // replaced rather than overwritten.
    private readonly nint[] _inSamples = new nint[InputSlots];
    private readonly nint[] _inBuffers = new nint[InputSlots];
    private readonly uint[] _inCapacity = new uint[InputSlots];
    private int _nextInput;

    // Timestamps submitted and not answered, oldest first.
    private readonly long[] _held = new long[MaxHeld + 2];
    private int _heldCount;

    private byte[] _parameterSets = [];
    private int _parameterSetsLength;
    private NvCodec _codec;
    private DecoderCounters _counters;

    /// <summary>A decoder that writes into <paramref name="picture"/>.</summary>
    /// <param name="gpu">The shared device.</param>
    /// <param name="picture">Where decoded pictures go (shared by the generation's decoders).</param>
    /// <param name="allowHardware">False forces CPU decoding even with a video-capable device.</param>
    public MfDecoder(GpuDevice gpu, TilePicture picture, bool allowHardware)
    {
        _gpu = gpu;
        _picture = picture;
        _allowHardware = allowHardware;
    }

    /// <summary>The decoder MFT's friendly name, once configured.</summary>
    public string Name { get; private set; } = "";

    /// <summary>Where pictures are decoded (known after the first picture).</summary>
    public DecoderKind Kind { get; private set; }

    /// <summary>True when the MFT took the device manager (GPU decoding requested).</summary>
    public bool UsesDevice => _d3d;

    /// <summary>Why the last Configure failed, for logs and reports.</summary>
    public string? ConfigureError { get; private set; }

    /// <summary>
    /// True when the last Configure failed because the system has no decoder
    /// for the codec at all (for H.265: the HEVC Video Extension is missing).
    /// </summary>
    public bool NoDecoderInstalled { get; private set; }

    /// <summary>The decoder's output size (may be the coded size, larger than the visible picture).</summary>
    public NvSize OutputSize => new(_outWidth, _outHeight);

    /// <summary>The last failing HRESULT from ProcessInput or ProcessOutput (0 if none), for reports.</summary>
    public int LastError { get; private set; }

    /// <summary>Counters since creation.</summary>
    public DecoderCounters Counters => _counters;

    /// <summary>
    /// Creates the decoder for a format, replacing any existing one.
    /// <paramref name="parameterSets"/> are Annex-B (start codes included),
    /// put in front of keyframes that do not carry their own. Returns the
    /// failure to report, or null.
    /// </summary>
    public NvDecodeFailure? Configure(NvCodec codec, ReadOnlySpan<byte> parameterSets, NvSize codedSize)
    {
        ReleaseTransform();
        ConfigureError = null;
        NoDecoderInstalled = false;
        if (codec != NvCodec.H264 && codec != NvCodec.H265)
        {
            ConfigureError = $"unknown codec {(int)codec}";
            return NvDecodeFailure.InvalidParameterSets;
        }
        _codec = codec;
        if (_parameterSets.Length < parameterSets.Length)
        {
            _parameterSets = new byte[parameterSets.Length];
        }
        parameterSets.CopyTo(_parameterSets);
        _parameterSetsLength = parameterSets.Length;

        Com.EnsureMta();
        if (!CreateTransform(codec))
        {
            return NvDecodeFailure.SessionCreateFailed;
        }
        IMFAttributes* attributes = null;
        bool d3dAware = false;
        if ((int)_mft->GetAttributes(&attributes) >= 0 && attributes != null)
        {
            // Output as soon as a picture is decoded, not after a reorder window.
            _ = attributes->SetUINT32(Ref(in MF_LOW_LATENCY), 1);
            uint aware = 0;
            d3dAware = (int)attributes->GetUINT32(Ref(in MF_SA_D3D11_AWARE), &aware) >= 0 && aware != 0;
            attributes->Release();
        }
        if (_allowHardware && d3dAware && _gpu.Manager != null)
        {
            _d3d = (int)_mft->ProcessMessage(
                MFT_MESSAGE_TYPE.MFT_MESSAGE_SET_D3D_MANAGER, (nuint)_gpu.Manager) >= 0;
        }

        ApplySettings(codedSize);
        HRESULT hr = SetInputType(codec, codedSize);
        if ((int)hr < 0 && _d3d)
        {
            // The GPU cannot take this format (profile, size): decode on the CPU.
            _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_SET_D3D_MANAGER, 0);
            _d3d = false;
            hr = SetInputType(codec, codedSize);
        }
        if ((int)hr < 0)
        {
            ConfigureError = $"{Name}: input type refused (0x{(int)hr:X8})";
            ReleaseTransform();
            return NvDecodeFailure.SessionCreateFailed;
        }
        // The H.265 decoder refuses its output type without a frame size.
        if (!NegotiateOutput())
        {
            ConfigureError = $"{Name}: no NV12 output (offers {OutputTypes()})";
            ReleaseTransform();
            return NvDecodeFailure.SessionCreateFailed;
        }
        _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
        _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        Kind = _d3d ? DecoderKind.Unknown : DecoderKind.Software;
        return null;
    }

    /// <summary>
    /// Submits one access unit (Annex-B, as the core delivered it; the
    /// bytes are copied before this returns) and drains every picture the
    /// decoder has ready, answering through <paramref name="results"/>.
    /// </summary>
    public void Decode(ReadOnlySpan<byte> accessUnit, bool keyframe, long ptsUs, IDecodeResults results)
    {
        if (_mft == null)
        {
            FailCurrent(results, NvDecodeFailure.NotConfigured, ptsUs);
            return;
        }
        if (accessUnit.IsEmpty)
        {
            FailCurrent(results, NvDecodeFailure.InvalidAccessUnit, ptsUs);
            return;
        }
        bool prepend = keyframe && _parameterSetsLength > 0 && !AnnexB.HasSps(accessUnit, _codec);
        int length = accessUnit.Length + (prepend ? _parameterSetsLength : 0);
        IMFSample* sample = TakeInput((uint)length, out IMFMediaBuffer* buffer);
        if (sample == null)
        {
            FailCurrent(results, NvDecodeFailure.SubmitFailed, ptsUs);
            return;
        }
        byte* dst;
        uint max, cur;
        if ((int)buffer->Lock(&dst, &max, &cur) < 0)
        {
            FailCurrent(results, NvDecodeFailure.SubmitFailed, ptsUs);
            return;
        }
        var span = new Span<byte>(dst, length);
        if (prepend)
        {
            _parameterSets.AsSpan(0, _parameterSetsLength).CopyTo(span);
            accessUnit.CopyTo(span[_parameterSetsLength..]);
        }
        else
        {
            accessUnit.CopyTo(span);
        }
        _ = buffer->Unlock();
        _ = buffer->SetCurrentLength((uint)length);
        // Media Foundation time is in 100 ns units.
        _ = sample->SetSampleTime(ptsUs * 10);
        _ = sample->SetUINT32(Ref(in MFSampleExtension_CleanPoint), keyframe ? 1u : 0u);

        HRESULT hr = _mft->ProcessInput(0, sample, 0);
        if ((int)hr == MF_E_NOTACCEPTING)
        {
            Drain(results);
            hr = _mft->ProcessInput(0, sample, 0);
        }
        if ((int)hr < 0)
        {
            LastError = (int)hr;
            FailCurrent(results, NvDecodeFailure.SubmitFailed, ptsUs);
            return;
        }
        _counters.Submitted++;
        if (_heldCount == _held.Length)
        {
            // Cannot happen (MaxHeld is enforced below); keep the array bounded anyway.
            DropOldest(results);
        }
        _held[_heldCount++] = ptsUs;
        Drain(results);
        while (_heldCount > MaxHeld)
        {
            _counters.HeldTooLong++;
            DropOldest(results);
        }
    }

    /// <summary>
    /// Answers every picture still in flight with <paramref name="kind"/>
    /// (before the decoder is thrown away outside the core's actions, as on
    /// GPU device loss, so no Decode is left without its answer).
    /// </summary>
    public void AbandonHeld(IDecodeResults results, NvDecodeFailure kind)
    {
        while (_heldCount > 0)
        {
            DropOldest(results, kind);
        }
    }

    /// <summary>Drops everything in flight (no answers for it) and keeps the decoder.</summary>
    public void Flush()
    {
        if (_mft != null)
        {
            _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        _heldCount = 0;
    }

    /// <summary>Releases the MFT and its samples (the picture stays).</summary>
    public void Dispose()
    {
        ReleaseTransform();
        for (int i = 0; i < InputSlots; i++)
        {
            ReleaseInput(i);
        }
    }

    private bool CreateTransform(NvCodec codec)
    {
        MFT_REGISTER_TYPE_INFO input = new()
        {
            guidMajorType = MFMediaType_Video,
            guidSubtype = codec == NvCodec.H264 ? MFVideoFormat_H264 : MFVideoFormat_HEVC,
        };
        MFT_REGISTER_TYPE_INFO output = new() { guidMajorType = MFMediaType_Video, guidSubtype = MFVideoFormat_NV12 };
        IMFActivate** activates = null;
        uint count = 0;
        uint flags = (uint)(_MFT_ENUM_FLAG.MFT_ENUM_FLAG_SYNCMFT | _MFT_ENUM_FLAG.MFT_ENUM_FLAG_LOCALMFT |
            _MFT_ENUM_FLAG.MFT_ENUM_FLAG_SORTANDFILTER);
        HRESULT hr = MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, flags, &input, &output, &activates, &count);
        if ((int)hr < 0 || count == 0)
        {
            ConfigureError = $"no {(codec == NvCodec.H264 ? "H.264" : "H.265")} decoder (0x{(int)hr:X8}, {count} found)";
            NoDecoderInstalled = (int)hr >= 0 && count == 0;
            if (activates != null)
            {
                CoTaskMemFree(activates);
            }
            return false;
        }
        HRESULT last = S_OK;
        for (uint i = 0; i < count; i++)
        {
            IMFActivate* a = activates[i];
            if (_mft == null)
            {
                IMFTransform* mft = null;
                last = a->ActivateObject(__uuidof<IMFTransform>(), (void**)&mft);
                if ((int)last >= 0)
                {
                    _mft = mft;
                    Name = FriendlyName(a);
                }
            }
            a->Release();
        }
        CoTaskMemFree(activates);
        if (_mft == null)
        {
            ConfigureError = $"decoder activation failed (0x{(int)last:X8})";
            return false;
        }
        return true;
    }

    private static string FriendlyName(IMFActivate* a)
    {
        char* name = null;
        uint length = 0;
        if ((int)a->GetAllocatedString(Ref(in MFT_FRIENDLY_NAME_Attribute), &name, &length) < 0 || name == null)
        {
            return "unnamed decoder";
        }
        string s = new(name, 0, (int)length);
        CoTaskMemFree(name);
        return s;
    }

    // DecoderSettings, applied before the input type is set (the decoder
    // sizes its surfaces from them when streaming begins).
    private void ApplySettings(NvSize codedSize)
    {
        if (_d3d && DecoderSettings.OutputSamples > 0)
        {
            IMFAttributes* output = null;
            if ((int)_mft->GetOutputStreamAttributes(0, &output) >= 0 && output != null)
            {
                _ = output->SetUINT32(Ref(in MF_SA_MINIMUM_OUTPUT_SAMPLE_COUNT), (uint)DecoderSettings.OutputSamples);
                output->Release();
            }
        }
        if (DecoderSettings.LimitCodedSize && codedSize.Width > 0 && codedSize.Height > 0)
        {
            ICodecAPI* api = null;
            if ((int)_mft->QueryInterface(__uuidof<ICodecAPI>(), (void**)&api) >= 0 && api != null)
            {
                VARIANT v = default;
                v.vt = (ushort)VARENUM.VT_UI4;
                v.ulVal = codedSize.Width;
                _ = api->SetValue(__uuidof<CODECAPI_AVDecVideoMaxCodedWidth>(), &v);
                v.ulVal = codedSize.Height;
                _ = api->SetValue(__uuidof<CODECAPI_AVDecVideoMaxCodedHeight>(), &v);
                api->Release();
            }
        }
    }

    private HRESULT SetInputType(NvCodec codec, NvSize codedSize)
    {
        IMFMediaType* type = null;
        HRESULT hr = MFCreateMediaType(&type);
        if ((int)hr < 0)
        {
            return hr;
        }
        Guid major = MFMediaType_Video;
        Guid subtype = codec == NvCodec.H264 ? MFVideoFormat_H264 : MFVideoFormat_HEVC;
        _ = type->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &major);
        _ = type->SetGUID(Ref(in MF_MT_SUBTYPE), &subtype);
        if (codedSize.Width > 0 && codedSize.Height > 0)
        {
            _ = MFSetAttributeSize((IMFAttributes*)type, Ref(in MF_MT_FRAME_SIZE), codedSize.Width, codedSize.Height);
        }
        _ = type->SetUINT32(Ref(in MF_MT_INTERLACE_MODE), (uint)MFVideoInterlaceMode.MFVideoInterlace_Progressive);
        hr = _mft->SetInputType(0, type, 0);
        type->Release();
        return hr;
    }

    // The output subtypes on offer, as FourCCs, for error messages.
    private string OutputTypes()
    {
        var names = new List<string>();
        for (uint i = 0; i < 32; i++)
        {
            IMFMediaType* type = null;
            HRESULT hr = _mft->GetOutputAvailableType(0, i, &type);
            if ((int)hr < 0)
            {
                if (names.Count == 0)
                {
                    names.Add($"none, 0x{(int)hr:X8}");
                }
                break;
            }
            Guid subtype;
            if ((int)type->GetGUID(Ref(in MF_MT_SUBTYPE), &subtype) >= 0)
            {
                uint fourcc = (uint)BitConverter.ToInt32(subtype.ToByteArray(), 0);
                names.Add(string.Concat(Enumerable.Range(0, 4).Select(k => (char)((fourcc >> (8 * k)) & 0xFF))));
            }
            type->Release();
        }
        return string.Join(" ", names);
    }

    // Picks NV12 output and reads its size and the output stream's needs.
    private bool NegotiateOutput()
    {
        Guid nv12 = MFVideoFormat_NV12;
        bool set = false;
        for (uint i = 0; !set; i++)
        {
            IMFMediaType* type = null;
            if ((int)_mft->GetOutputAvailableType(0, i, &type) < 0)
            {
                break;
            }
            Guid subtype;
            if ((int)type->GetGUID(Ref(in MF_MT_SUBTYPE), &subtype) >= 0 && subtype == nv12 &&
                (int)_mft->SetOutputType(0, type, 0) >= 0)
            {
                uint w = 0, h = 0, stride = 0;
                _ = MFGetAttributeSize((IMFAttributes*)type, Ref(in MF_MT_FRAME_SIZE), &w, &h);
                _outWidth = w;
                _outHeight = h;
                _defaultStride = (int)type->GetUINT32(Ref(in MF_MT_DEFAULT_STRIDE), &stride) >= 0
                    ? (int)stride : (int)w;
                set = true;
            }
            type->Release();
        }
        if (!set)
        {
            return false;
        }
        MFT_OUTPUT_STREAM_INFO info;
        if ((int)_mft->GetOutputStreamInfo(0, &info) < 0)
        {
            return false;
        }
        uint provides = (uint)(_MFT_OUTPUT_STREAM_INFO_FLAGS.MFT_OUTPUT_STREAM_PROVIDES_SAMPLES |
            _MFT_OUTPUT_STREAM_INFO_FLAGS.MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES);
        _providesSamples = (info.dwFlags & provides) != 0;
        if (!_providesSamples && (_outSample == null || _outSampleSize < info.cbSize))
        {
            ReleaseOutputSample();
            IMFSample* sample = null;
            IMFMediaBuffer* buffer = null;
            uint alignment = info.cbAlignment > 0 ? info.cbAlignment - 1 : 0;
            if ((int)MFCreateSample(&sample) < 0)
            {
                return false;
            }
            if ((int)MFCreateAlignedMemoryBuffer(info.cbSize, alignment, &buffer) < 0)
            {
                sample->Release();
                return false;
            }
            _ = sample->AddBuffer(buffer);
            _outSample = sample;
            _outBuffer = buffer;
            _outSampleSize = info.cbSize;
        }
        return true;
    }

    // Takes every picture the decoder has ready.
    private void Drain(IDecodeResults results)
    {
        while (true)
        {
            MFT_OUTPUT_DATA_BUFFER output = default;
            output.dwStreamID = 0;
            output.pSample = _providesSamples ? null : _outSample;
            if (!_providesSamples)
            {
                // The H.264 decoder writes after the buffer's current length
                // (measured: the first picture repeats, then E_FAIL when full).
                _ = _outBuffer->SetCurrentLength(0);
            }
            uint status = 0;
            HRESULT hr = _mft->ProcessOutput(0, 1, &output, &status);
            if (output.pEvents != null)
            {
                output.pEvents->Release();
            }
            if ((int)hr == MF_E_TRANSFORM_NEED_MORE_INPUT)
            {
                return;
            }
            if ((int)hr == MF_E_TRANSFORM_STREAM_CHANGE)
            {
                if (_providesSamples && output.pSample != null)
                {
                    output.pSample->Release();
                }
                if (!NegotiateOutput())
                {
                    DropOldest(results, NvDecodeFailure.DecodeError);
                    return;
                }
                continue;
            }
            if ((int)hr < 0)
            {
                LastError = (int)hr;
                DropOldest(results, NvDecodeFailure.DecodeError);
                return;
            }
            IMFSample* picture = output.pSample;
            if (picture == null)
            {
                continue;
            }
            long time = 0;
            _ = picture->GetSampleTime(&time);
            long pts = time / 10;
            bool published = Publish(picture, pts);
            if (_providesSamples)
            {
                picture->Release();
            }
            if (!published)
            {
                // A failure answers the oldest picture in flight.
                DropOldest(results, NvDecodeFailure.DecodeError);
                continue;
            }
            if (!TakeHeld(pts, out long answered))
            {
                _counters.Unmatched++;
                continue;
            }
            _counters.Pictures++;
            results.Decoded(answered);
        }
    }

    // Puts one output picture into the tile's picture.
    private bool Publish(IMFSample* sample, long pts)
    {
        IMFMediaBuffer* buffer = null;
        if ((int)sample->GetBufferByIndex(0, &buffer) < 0)
        {
            return false;
        }
        try
        {
            IMFDXGIBuffer* dxgi = null;
            if (_d3d && (int)buffer->QueryInterface(__uuidof<IMFDXGIBuffer>(), (void**)&dxgi) >= 0)
            {
                ID3D11Texture2D* texture = null;
                uint subresource = 0;
                bool ok = (int)dxgi->GetResource(__uuidof<ID3D11Texture2D>(), (void**)&texture) >= 0 &&
                    (int)dxgi->GetSubresourceIndex(&subresource) >= 0;
                dxgi->Release();
                if (!ok)
                {
                    if (texture != null)
                    {
                        texture->Release();
                    }
                    return false;
                }
                if (Kind == DecoderKind.Unknown)
                {
                    D3D11_TEXTURE2D_DESC desc;
                    texture->GetDesc(&desc);
                    Kind = (desc.BindFlags & (uint)D3D11_BIND_FLAG.D3D11_BIND_DECODER) != 0
                        ? DecoderKind.Hardware : DecoderKind.SoftwareTexture;
                }
                using (_gpu.Lock())
                {
                    _picture.CopyFrom(texture, subresource, _outWidth, _outHeight, pts);
                }
                texture->Release();
                return true;
            }
            return Upload(buffer, pts);
        }
        finally
        {
            buffer->Release();
        }
    }

    private bool Upload(IMFMediaBuffer* buffer, long pts)
    {
        IMF2DBuffer* two = null;
        byte* data = null;
        int pitch;
        if ((int)buffer->QueryInterface(__uuidof<IMF2DBuffer>(), (void**)&two) >= 0)
        {
            if ((int)two->Lock2D(&data, &pitch) < 0)
            {
                two->Release();
                return false;
            }
            UploadLocked(data, pitch, pts);
            _ = two->Unlock2D();
            two->Release();
        }
        else
        {
            uint max, length;
            if ((int)buffer->Lock(&data, &max, &length) < 0)
            {
                return false;
            }
            pitch = _defaultStride > 0 ? _defaultStride : (int)_outWidth;
            UploadLocked(data, pitch, pts);
            _ = buffer->Unlock();
        }
        Kind = DecoderKind.Software;
        return true;
    }

    private void UploadLocked(byte* data, int pitch, long pts)
    {
        using (_gpu.Lock())
        {
            _picture.Upload(data, pitch, pitch * (int)_outHeight, _outWidth, _outHeight, pts);
        }
    }

    // Removes the answered timestamp from the held list: the one that came
    // out if it is there, otherwise the oldest.
    private bool TakeHeld(long pts, out long answered)
    {
        if (_heldCount == 0)
        {
            answered = 0;
            return false;
        }
        int index = 0;
        for (int i = 0; i < _heldCount; i++)
        {
            if (_held[i] == pts)
            {
                index = i;
                break;
            }
        }
        answered = _held[index];
        RemoveHeld(index);
        return true;
    }

    // A failure answers the core's oldest Decode in flight (docs/RENDERING.md),
    // which is this one only when nothing is held: keep the held list the
    // same as the core's by dropping the oldest and keeping this one.
    private void FailCurrent(IDecodeResults results, NvDecodeFailure kind, long ptsUs)
    {
        if (_heldCount > 0)
        {
            RemoveHeld(0);
            _held[_heldCount++] = ptsUs;
        }
        results.Failed(kind);
    }

    private void DropOldest(IDecodeResults results, NvDecodeFailure kind = NvDecodeFailure.NoPicture)
    {
        if (_heldCount == 0)
        {
            return;
        }
        RemoveHeld(0);
        results.Failed(kind);
    }

    private void RemoveHeld(int index)
    {
        for (int i = index + 1; i < _heldCount; i++)
        {
            _held[i - 1] = _held[i];
        }
        _heldCount--;
    }

    // The next input sample with room for `length` bytes.
    private IMFSample* TakeInput(uint length, out IMFMediaBuffer* buffer)
    {
        int slot = _nextInput;
        _nextInput = (slot + 1) % InputSlots;
        var sample = (IMFSample*)_inSamples[slot];
        if (sample != null)
        {
            // Only our reference left? (AddRef then Release returns the count.)
            sample->AddRef();
            if (sample->Release() > 1)
            {
                ReleaseInput(slot);
                sample = null;
            }
        }
        if (sample != null && _inCapacity[slot] < length)
        {
            ReleaseInput(slot);
            sample = null;
        }
        if (sample == null)
        {
            // Room for the largest access unit seen so far, rounded up.
            uint capacity = Math.Max(length, 64u * 1024);
            capacity = (capacity + 0xFFFF) & ~0xFFFFu;
            IMFMediaBuffer* fresh = null;
            if ((int)MFCreateSample(&sample) < 0)
            {
                buffer = null;
                return null;
            }
            if ((int)MFCreateMemoryBuffer(capacity, &fresh) < 0)
            {
                sample->Release();
                buffer = null;
                return null;
            }
            _ = sample->AddBuffer(fresh);
            _inSamples[slot] = (nint)sample;
            _inBuffers[slot] = (nint)fresh;
            _inCapacity[slot] = capacity;
            _counters.InputAllocations++;
        }
        buffer = (IMFMediaBuffer*)_inBuffers[slot];
        return sample;
    }

    private void ReleaseInput(int slot)
    {
        if (_inBuffers[slot] != 0)
        {
            ((IMFMediaBuffer*)_inBuffers[slot])->Release();
            _inBuffers[slot] = 0;
        }
        if (_inSamples[slot] != 0)
        {
            ((IMFSample*)_inSamples[slot])->Release();
            _inSamples[slot] = 0;
        }
        _inCapacity[slot] = 0;
    }

    private void ReleaseOutputSample()
    {
        if (_outSample != null)
        {
            _outBuffer->Release();
            _outBuffer = null;
            _outSample->Release();
            _outSample = null;
            _outSampleSize = 0;
        }
    }

    private void ReleaseTransform()
    {
        if (_mft != null)
        {
            _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_COMMAND_FLUSH, 0);
            if (_d3d)
            {
                _ = _mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_SET_D3D_MANAGER, 0);
            }
            _mft->Release();
            _mft = null;
        }
        ReleaseOutputSample();
        _d3d = false;
        _heldCount = 0;
        _outWidth = 0;
        _outHeight = 0;
        Kind = DecoderKind.Unknown;
    }

    // TerraFX exposes GUIDs as `ref readonly` properties over static data.
    private static Guid* Ref(in Guid g) => (Guid*)Unsafe.AsPointer(ref Unsafe.AsRef(in g));
}

/// <summary>Annex-B helpers.</summary>
public static class AnnexB
{
    /// <summary>True when the access unit carries a sequence parameter set.</summary>
    public static bool HasSps(ReadOnlySpan<byte> au, NvCodec codec)
    {
        for (int i = 0; i + 3 < au.Length; i++)
        {
            if (au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1)
            {
                byte header = au[i + 3];
                int type = codec == NvCodec.H264 ? header & 0x1F : (header >> 1) & 0x3F;
                if (type == (codec == NvCodec.H264 ? 7 : 33))
                {
                    return true;
                }
                i += 3;
            }
        }
        return false;
    }

    /// <summary>
    /// Writes NAL units (without start codes) as Annex-B into
    /// <paramref name="output"/>, growing it as needed; returns the length.
    /// </summary>
    public static int Join(ReadOnlySpan<NvBytes> nals, ref byte[] output)
    {
        int length = 0;
        foreach (NvBytes nal in nals)
        {
            length += 4 + checked((int)nal.Len);
        }
        if (output.Length < length)
        {
            output = new byte[length];
        }
        int at = 0;
        foreach (NvBytes nal in nals)
        {
            output[at] = 0;
            output[at + 1] = 0;
            output[at + 2] = 0;
            output[at + 3] = 1;
            nal.AsSpan().CopyTo(output.AsSpan(at + 4));
            at += 4 + (int)nal.Len;
        }
        return length;
    }
}
