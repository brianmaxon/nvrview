// Turns one cover keyframe (H.264 or H.265, Annex-B with its parameter sets,
// as a recording's cover thumbnail arrives from the core) into a small JPEG:
// decoded with the system's Media Foundation decoder, shrunk to at most 320
// pixels on the longer side (ThumbnailSizing), and encoded with WIC without
// metadata. Synchronous and stateless: call it off the UI thread (the app's
// ThumbnailDecodeQueue does). Like the macOS KeyframeImage.
//
// The decode runs on the CPU: the MFT is given no device manager, so one
// picture costs no GPU device, no DXVA session, and no readback. (The tiles
// use the GPU path, MfDecoder; a cover is one frame, not a stream.)

using System.Runtime.CompilerServices;
using NvrView.Interop;
using NvrView.Shell;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.MFT;
using static TerraFX.Interop.Windows.MFVideoFormat;

namespace NvrView.Video;

/// <summary>A decoded NV12 picture in managed memory.</summary>
/// <param name="Data">The luma plane (rows of <paramref name="Pitch"/> bytes) and then the chroma plane.</param>
/// <param name="Pitch">Bytes per row.</param>
/// <param name="ChromaOffset">Where the chroma plane starts.</param>
/// <param name="Width">The visible width.</param>
/// <param name="Height">The visible height.</param>
/// <param name="Matrix">The matrix the decoder reported, or the guess for the size.</param>
/// <param name="FullRange">True for 0-255 luma.</param>
/// <param name="Decoder">The decoder's name, for logs.</param>
public sealed record Nv12Picture(byte[] Data, int Pitch, int ChromaOffset, int Width, int Height, YuvMatrix Matrix, bool FullRange, string Decoder);

/// <summary>A cover keyframe to a small JPEG (see the file comment).</summary>
public static unsafe class KeyframeImage
{
    private static int s_started;

    // GUIDs used here that the bindings do not expose as constants we can rely on.
    private static readonly Guid ClsidWicImagingFactory = new("cacaf262-9370-4615-a13b-9f5539da4c0a");
    private static readonly Guid ContainerFormatJpeg = new("19e4a5aa-5662-4fc5-a0c0-1758028e1057");
    private static readonly Guid PixelFormat24bppBgr = new("6fddc324-4e03-4bfe-b185-3d77768dc90c");
    private static readonly Guid AttrYuvMatrix = new("e2724bb8-e676-4806-b4b2-a8d6efb44ccd");
    private static readonly Guid AttrNominalRange = new("c21b8ee5-b956-4071-8daf-325edf5cab11");

    /// <summary>
    /// Decodes <paramref name="keyframe"/> to a JPEG of at most
    /// <see cref="ThumbnailSizing.MaxSide"/> pixels on its longer side, as an
    /// <see cref="NvThumbKind.Image"/> thumbnail. Null with a reason in
    /// <paramref name="error"/> when it does not decode.
    /// </summary>
    public static RecordedThumbnail? Thumbnail(RecordedThumbnail keyframe, out string? error)
    {
        NvCodec codec = keyframe.Kind switch
        {
            NvThumbKind.H264Keyframe => NvCodec.H264,
            NvThumbKind.H265Keyframe => NvCodec.H265,
            _ => 0,
        };
        if (codec == 0)
        {
            error = "not a keyframe";
            return null;
        }
        Nv12Picture? picture = Decode(codec, keyframe.Data, keyframe.Width, keyframe.Height, out error);
        if (picture is null)
        {
            return null;
        }
        (int w, int h) = ThumbnailSizing.Fit(picture.Width, picture.Height);
        byte[] bgr = Nv12Scaler.ToBgr24(picture.Data, picture.Pitch, picture.ChromaOffset, picture.Width, picture.Height,
            w, h, picture.Matrix, picture.FullRange);
        byte[]? jpeg = EncodeJpeg(bgr, w, h, out error);
        return jpeg is null ? null : new RecordedThumbnail(NvThumbKind.Image, (uint)w, (uint)h, keyframe.PtsUs, jpeg);
    }

    /// <summary>
    /// Decodes the first picture of an Annex-B keyframe (parameter sets in front).
    /// <paramref name="width"/> and <paramref name="height"/> are the keyframe's visible size when the core
    /// knows it (0: use the decoder's output size).
    /// </summary>
    public static Nv12Picture? Decode(NvCodec codec, ReadOnlySpan<byte> annexB, uint width, uint height, out string? error)
    {
        error = null;
        if (annexB.IsEmpty)
        {
            error = "empty keyframe";
            return null;
        }
        Com.EnsureMta();
        if (Interlocked.CompareExchange(ref s_started, 1, 0) == 0)
        {
            _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
        }
        IMFTransform* mft = null;
        IMFSample* input = null;
        IMFMediaBuffer* inputBuffer = null;
        IMFSample* ownOutput = null;
        try
        {
            string name = Create(codec, &mft, out error);
            if (mft == null)
            {
                return null;
            }
            IMFAttributes* attributes = null;
            if ((int)mft->GetAttributes(&attributes) >= 0 && attributes != null)
            {
                _ = attributes->SetUINT32(Ref(in MF_LOW_LATENCY), 1);
                attributes->Release();
            }
            if (!SetInput(mft, codec, width, height, out error) || !SetOutput(mft, out Output output, out error))
            {
                return null;
            }
            _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);

            if ((int)MFCreateSample(&input) < 0 || (int)MFCreateMemoryBuffer((uint)annexB.Length, &inputBuffer) < 0)
            {
                error = "no memory for the keyframe";
                return null;
            }
            byte* dst;
            uint max, cur;
            if ((int)inputBuffer->Lock(&dst, &max, &cur) < 0)
            {
                error = "the input buffer could not be locked";
                return null;
            }
            annexB.CopyTo(new Span<byte>(dst, annexB.Length));
            _ = inputBuffer->Unlock();
            _ = inputBuffer->SetCurrentLength((uint)annexB.Length);
            _ = input->AddBuffer(inputBuffer);
            _ = input->SetSampleTime(0);
            _ = input->SetUINT32(Ref(in MFSampleExtension_CleanPoint), 1);
            HRESULT hr = mft->ProcessInput(0, input, 0);
            if ((int)hr < 0)
            {
                error = $"{name}: the keyframe was refused (0x{(int)hr:X8})";
                return null;
            }
            // One picture is all that is wanted: have the decoder give up what it holds.
            _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_COMMAND_DRAIN, 0);

            for (int attempt = 0; attempt < 8; attempt++)
            {
                MFT_OUTPUT_DATA_BUFFER buffer = default;
                if (!output.ProvidesSamples)
                {
                    if (ownOutput == null && !MakeOutputSample(mft, &ownOutput))
                    {
                        error = "no memory for the picture";
                        return null;
                    }
                    IMFMediaBuffer* b = null;
                    if ((int)ownOutput->GetBufferByIndex(0, &b) >= 0 && b != null)
                    {
                        _ = b->SetCurrentLength(0);
                        b->Release();
                    }
                    buffer.pSample = ownOutput;
                }
                uint status = 0;
                hr = mft->ProcessOutput(0, 1, &buffer, &status);
                if (buffer.pEvents != null)
                {
                    buffer.pEvents->Release();
                }
                if ((int)hr == MF_E_TRANSFORM_STREAM_CHANGE)
                {
                    if (output.ProvidesSamples && buffer.pSample != null)
                    {
                        buffer.pSample->Release();
                    }
                    if (!SetOutput(mft, out output, out error))
                    {
                        return null;
                    }
                    continue;
                }
                if ((int)hr == MF_E_TRANSFORM_NEED_MORE_INPUT)
                {
                    error = $"{name}: no picture came out";
                    return null;
                }
                if ((int)hr < 0)
                {
                    error = $"{name}: decoding failed (0x{(int)hr:X8})";
                    return null;
                }
                IMFSample* sample = buffer.pSample;
                try
                {
                    return Copy(sample, output, width, height, name, out error);
                }
                finally
                {
                    if (output.ProvidesSamples && sample != null)
                    {
                        sample->Release();
                    }
                }
            }
            error = $"{name}: the output kept changing";
            return null;
        }
        finally
        {
            if (ownOutput != null)
            {
                ownOutput->Release();
            }
            if (inputBuffer != null)
            {
                inputBuffer->Release();
            }
            if (input != null)
            {
                input->Release();
            }
            if (mft != null)
            {
                _ = mft->ProcessMessage(MFT_MESSAGE_TYPE.MFT_MESSAGE_COMMAND_FLUSH, 0);
                mft->Release();
            }
        }
    }

    private struct Output
    {
        public uint Width;
        public uint Height;
        public int Stride;
        public bool ProvidesSamples;
        public YuvMatrix? Matrix;
        public bool? FullRange;
    }

    // The system's software decoder for the codec (the best merit first).
    private static string Create(NvCodec codec, IMFTransform** mft, out string? error)
    {
        error = null;
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
            error = $"no {(codec == NvCodec.H264 ? "H.264" : "H.265")} decoder (0x{(int)hr:X8}, {count} found)";
            if (activates != null)
            {
                CoTaskMemFree(activates);
            }
            return "";
        }
        string name = "";
        HRESULT last = S_OK;
        for (uint i = 0; i < count; i++)
        {
            IMFActivate* a = activates[i];
            if (*mft == null)
            {
                IMFTransform* made = null;
                last = a->ActivateObject(__uuidof<IMFTransform>(), (void**)&made);
                if ((int)last >= 0)
                {
                    *mft = made;
                    char* text = null;
                    uint length = 0;
                    if ((int)a->GetAllocatedString(Ref(in MFT_FRIENDLY_NAME_Attribute), &text, &length) >= 0 && text != null)
                    {
                        name = new string(text, 0, (int)length);
                        CoTaskMemFree(text);
                    }
                }
            }
            a->Release();
        }
        CoTaskMemFree(activates);
        if (*mft == null)
        {
            error = $"decoder activation failed (0x{(int)last:X8})";
        }
        return name;
    }

    private static bool SetInput(IMFTransform* mft, NvCodec codec, uint width, uint height, out string? error)
    {
        error = null;
        IMFMediaType* type = null;
        if ((int)MFCreateMediaType(&type) < 0)
        {
            error = "no memory for the input type";
            return false;
        }
        Guid major = MFMediaType_Video;
        Guid subtype = codec == NvCodec.H264 ? MFVideoFormat_H264 : MFVideoFormat_HEVC;
        _ = type->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &major);
        _ = type->SetGUID(Ref(in MF_MT_SUBTYPE), &subtype);
        if (width > 0 && height > 0)
        {
            _ = MFSetAttributeSize((IMFAttributes*)type, Ref(in MF_MT_FRAME_SIZE), width, height);
        }
        _ = type->SetUINT32(Ref(in MF_MT_INTERLACE_MODE), (uint)MFVideoInterlaceMode.MFVideoInterlace_Progressive);
        HRESULT hr = mft->SetInputType(0, type, 0);
        type->Release();
        if ((int)hr < 0)
        {
            error = $"the decoder refused the input type (0x{(int)hr:X8})";
            return false;
        }
        return true;
    }

    // Chooses NV12 output and reads what the decoder says about it.
    private static bool SetOutput(IMFTransform* mft, out Output output, out string? error)
    {
        error = null;
        output = default;
        Guid nv12 = MFVideoFormat_NV12;
        bool set = false;
        for (uint i = 0; !set; i++)
        {
            IMFMediaType* type = null;
            if ((int)mft->GetOutputAvailableType(0, i, &type) < 0)
            {
                break;
            }
            Guid subtype;
            if ((int)type->GetGUID(Ref(in MF_MT_SUBTYPE), &subtype) >= 0 && subtype == nv12 &&
                (int)mft->SetOutputType(0, type, 0) >= 0)
            {
                uint w = 0, h = 0, stride = 0, value = 0;
                _ = MFGetAttributeSize((IMFAttributes*)type, Ref(in MF_MT_FRAME_SIZE), &w, &h);
                output.Width = w;
                output.Height = h;
                output.Stride = (int)type->GetUINT32(Ref(in MF_MT_DEFAULT_STRIDE), &stride) >= 0 ? (int)stride : (int)w;
                Guid matrixKey = AttrYuvMatrix, rangeKey = AttrNominalRange;
                if ((int)type->GetUINT32(&matrixKey, &value) >= 0)
                {
                    if (value == (uint)MFVideoTransferMatrix.MFVideoTransferMatrix_BT709)
                    {
                        output.Matrix = YuvMatrix.Bt709;
                    }
                    else if (value == (uint)MFVideoTransferMatrix.MFVideoTransferMatrix_BT601)
                    {
                        output.Matrix = YuvMatrix.Bt601;
                    }
                }
                if ((int)type->GetUINT32(&rangeKey, &value) >= 0)
                {
                    if (value == (uint)MFNominalRange.MFNominalRange_0_255)
                    {
                        output.FullRange = true;
                    }
                    else if (value == (uint)MFNominalRange.MFNominalRange_16_235)
                    {
                        output.FullRange = false;
                    }
                }
                set = true;
            }
            type->Release();
        }
        if (!set)
        {
            error = "the decoder offers no NV12 output";
            return false;
        }
        MFT_OUTPUT_STREAM_INFO info;
        if ((int)mft->GetOutputStreamInfo(0, &info) < 0)
        {
            error = "the decoder's output stream is unreadable";
            return false;
        }
        uint provides = (uint)(_MFT_OUTPUT_STREAM_INFO_FLAGS.MFT_OUTPUT_STREAM_PROVIDES_SAMPLES |
            _MFT_OUTPUT_STREAM_INFO_FLAGS.MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES);
        output.ProvidesSamples = (info.dwFlags & provides) != 0;
        return true;
    }

    private static bool MakeOutputSample(IMFTransform* mft, IMFSample** sample)
    {
        MFT_OUTPUT_STREAM_INFO info;
        if ((int)mft->GetOutputStreamInfo(0, &info) < 0)
        {
            return false;
        }
        IMFMediaBuffer* buffer = null;
        uint alignment = info.cbAlignment > 0 ? info.cbAlignment - 1 : 0;
        if ((int)MFCreateSample(sample) < 0)
        {
            return false;
        }
        if ((int)MFCreateAlignedMemoryBuffer(info.cbSize, alignment, &buffer) < 0)
        {
            (*sample)->Release();
            *sample = null;
            return false;
        }
        _ = (*sample)->AddBuffer(buffer);
        buffer->Release();
        return true;
    }

    // Copies the picture out of the sample into managed memory.
    private static Nv12Picture? Copy(IMFSample* sample, Output output, uint width, uint height, string name, out string? error)
    {
        error = null;
        if (sample == null)
        {
            error = $"{name}: the decoder gave no picture";
            return null;
        }
        IMFMediaBuffer* buffer = null;
        if ((int)sample->ConvertToContiguousBuffer(&buffer) < 0 || buffer == null)
        {
            error = $"{name}: the picture could not be read";
            return null;
        }
        try
        {
            int outHeight = (int)output.Height;
            int pitch = output.Stride > 0 ? output.Stride : (int)output.Width;
            byte* data = null;
            IMF2DBuffer* two = null;
            bool locked2D = false;
            if ((int)buffer->QueryInterface(__uuidof<IMF2DBuffer>(), (void**)&two) >= 0 && two != null)
            {
                int p;
                if ((int)two->Lock2D(&data, &p) >= 0)
                {
                    locked2D = true;
                    pitch = p;
                }
            }
            if (!locked2D)
            {
                uint max, length;
                if ((int)buffer->Lock(&data, &max, &length) < 0)
                {
                    if (two != null)
                    {
                        two->Release();
                    }
                    error = $"{name}: the picture could not be locked";
                    return null;
                }
            }
            try
            {
                if (pitch <= 0 || outHeight <= 0)
                {
                    error = $"{name}: the picture has no size";
                    return null;
                }
                int total = pitch * outHeight * 3 / 2;
                var copy = new byte[total];
                new ReadOnlySpan<byte>(data, total).CopyTo(copy);
                int w = width > 0 && width <= output.Width ? (int)width : (int)output.Width;
                int h = height > 0 && height <= output.Height ? (int)height : outHeight;
                YuvMatrix matrix = output.Matrix ?? Nv12Scaler.GuessMatrix(h);
                return new Nv12Picture(copy, pitch, pitch * outHeight, w, h, matrix, output.FullRange ?? false, name);
            }
            finally
            {
                if (locked2D)
                {
                    _ = two->Unlock2D();
                }
                else
                {
                    _ = buffer->Unlock();
                }
                if (two != null)
                {
                    two->Release();
                }
            }
        }
        finally
        {
            buffer->Release();
        }
    }

    /// <summary>Encodes 24-bit BGR rows (no padding) as a JPEG with WIC, without metadata.</summary>
    public static byte[]? EncodeJpeg(byte[] bgr, int width, int height, out string? error)
    {
        error = null;
        Com.EnsureMta();
        IWICImagingFactory* factory = null;
        IWICStream* stream = null;
        IWICBitmapEncoder* encoder = null;
        IWICBitmapFrameEncode* frame = null;
        IPropertyBag2* options = null;
        // Room for the worst case: a JPEG of a picture this small is never larger than its raw pixels.
        byte[] target = new byte[bgr.Length + 4096];
        try
        {
            Guid clsid = ClsidWicImagingFactory;
            HRESULT hr = CoCreateInstance(&clsid, null, (uint)CLSCTX.CLSCTX_INPROC_SERVER, __uuidof<IWICImagingFactory>(), (void**)&factory);
            if ((int)hr < 0)
            {
                error = $"WIC is unavailable (0x{(int)hr:X8})";
                return null;
            }
            fixed (byte* pixels = bgr)
            fixed (byte* memory = target)
            {
                Guid container = ContainerFormatJpeg;
                Guid format = PixelFormat24bppBgr;
                Guid wanted = format;
                if ((int)factory->CreateStream(&stream) < 0 || (int)stream->InitializeFromMemory(memory, (uint)target.Length) < 0 ||
                    (int)factory->CreateEncoder(&container, null, &encoder) < 0 ||
                    (int)encoder->Initialize((IStream*)stream, WICBitmapEncoderCacheOption.WICBitmapEncoderNoCache) < 0 ||
                    (int)encoder->CreateNewFrame(&frame, &options) < 0)
                {
                    error = "the JPEG encoder could not be set up";
                    return null;
                }
                SetQuality(options, 0.7f);
                if ((int)frame->Initialize(options) < 0 || (int)frame->SetSize((uint)width, (uint)height) < 0 ||
                    (int)frame->SetPixelFormat(&wanted) < 0 || wanted != format ||
                    (int)frame->WritePixels((uint)height, (uint)(width * 3), (uint)bgr.Length, pixels) < 0 ||
                    (int)frame->Commit() < 0 || (int)encoder->Commit() < 0)
                {
                    error = "the JPEG encoder failed";
                    return null;
                }
                ULARGE_INTEGER position;
                if ((int)((IStream*)stream)->Seek(default, 1, &position) < 0 || position.QuadPart == 0 || position.QuadPart > (ulong)target.Length)
                {
                    error = "the JPEG size is unknown";
                    return null;
                }
                return target.AsSpan(0, (int)position.QuadPart).ToArray();
            }
        }
        finally
        {
            if (options != null)
            {
                options->Release();
            }
            if (frame != null)
            {
                frame->Release();
            }
            if (encoder != null)
            {
                encoder->Release();
            }
            if (stream != null)
            {
                stream->Release();
            }
            if (factory != null)
            {
                factory->Release();
            }
        }
    }

    // The encoder's ImageQuality option (0 to 1); a failure leaves its default.
    private static void SetQuality(IPropertyBag2* options, float quality)
    {
        if (options == null)
        {
            return;
        }
        fixed (char* name = "ImageQuality")
        {
            PROPBAG2 property = default;
            property.pstrName = name;
            VARIANT value = default;
            value.vt = (ushort)VARENUM.VT_R4;
            value.fltVal = quality;
            _ = options->Write(1, &property, &value);
        }
    }

    // TerraFX exposes GUIDs as `ref readonly` properties over static data.
    private static Guid* Ref(in Guid g) => (Guid*)Unsafe.AsPointer(ref Unsafe.AsRef(in g));
}
