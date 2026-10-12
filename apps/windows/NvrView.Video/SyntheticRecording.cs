// Synthetic recordings for checking the follow cut and grids (docs/UX.md
// "Recordings", "Follow and Grid"): MP4 files written with Media Foundation's
// sink writer whose every picture says which camera it is and which frame it
// is. The top three quarters are one of Palette's colors; the bottom quarter is
// Bits black or white blocks across the width, the frame number in binary (most
// significant bit on the left). With Sound, a silent AAC track (48 kHz mono)
// runs along, for the sound of exports. Made up entirely; nothing comes from a device.
// The same pictures as the macOS app's SyntheticRecording.

using System.Runtime.CompilerServices;
using NvrView.Shell;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.MFAudioFormat;
using static TerraFX.Interop.Windows.MFVideoFormat;

namespace NvrView.Video;

/// <summary>Writes and reads synthetic recordings (see the file comment).</summary>
public static unsafe class SyntheticRecording
{
    /// <summary>The cameras' colors (RGB), one per source.</summary>
    public static IReadOnlyList<(int R, int G, int B)> Palette { get; } = [(230, 40, 40), (40, 200, 60), (40, 70, 230), (230, 210, 40)];

    // MFTranscodeContainerType_MPEG4.
    private static readonly Guid Mpeg4Container = new("dc6cd05d-b9d0-40ef-bd35-fa622c1ab28a");

    /// <summary>Blocks in the frame-number strip.</summary>
    public const int Bits = 10;

    /// <summary>What to write.</summary>
    /// <param name="Frames">Pictures (below 2^Bits).</param>
    /// <param name="Color">Index into <see cref="Palette"/>.</param>
    /// <param name="Width">Picture width (even).</param>
    /// <param name="Height">Picture height (a multiple of 4).</param>
    /// <param name="Fps">Pictures per second.</param>
    /// <param name="KeyframeInterval">Pictures from one keyframe to the next.</param>
    /// <param name="Sound">Also a silent AAC track as long as the pictures.</param>
    public readonly record struct Spec(
        int Frames, int Color, int Width = 640, int Height = 360, int Fps = 10, int KeyframeInterval = 20, bool Sound = false);

    /// <summary>The sound track's sample rate.</summary>
    public const int SoundRate = 48_000;

    /// <summary>Writes an H.264 MP4 (BT.601 by size, video range) at <paramref name="path"/>; throws <see cref="VideoException"/>.</summary>
    public static void Write(Spec spec, string path)
    {
        GpuDevice.StartMediaFoundation();
        Com.EnsureMta();
        string? dir = System.IO.Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(dir))
        {
            Directory.CreateDirectory(dir);
        }
        File.Delete(path);
        IMFAttributes* attributes = null;
        Check(MFCreateAttributes(&attributes, 2), "MFCreateAttributes");
        IMFSinkWriter* writer = null;
        try
        {
            // The software encoder: the same stream on every machine.
            _ = attributes->SetUINT32(Ref(in MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS), 0);
            _ = attributes->SetGUID(Ref(in MF_TRANSCODE_CONTAINERTYPE), Ref(in Mpeg4Container));
            fixed (char* url = path)
            {
                Check(MFCreateSinkWriterFromURL(url, null, attributes, &writer), "MFCreateSinkWriterFromURL");
            }
        }
        finally
        {
            attributes->Release();
        }
        try
        {
            WriteFrames(writer, spec);
        }
        finally
        {
            writer->Release();
        }
    }

    private static void WriteFrames(IMFSinkWriter* writer, Spec spec)
    {
        uint stream;
        IMFMediaType* output = VideoType(MFVideoFormat_H264, spec);
        try
        {
            _ = output->SetUINT32(Ref(in MF_MT_AVG_BITRATE), (uint)(spec.Width * spec.Height * spec.Fps * 2));
            Check(writer->AddStream(output, &stream), "AddStream");
        }
        finally
        {
            output->Release();
        }
        // The group of pictures and no reordering, before the input type creates the encoder.
        IMFSinkWriterEncoderConfig* config = null;
        if ((int)writer->QueryInterface(__uuidof<IMFSinkWriterEncoderConfig>(), (void**)&config) >= 0)
        {
            IMFAttributes* parameters = null;
            if ((int)MFCreateAttributes(&parameters, 2) >= 0)
            {
                _ = parameters->SetUINT32(__uuidof<CODECAPI_AVEncMPVGOPSize>(), (uint)spec.KeyframeInterval);
                _ = parameters->SetUINT32(__uuidof<CODECAPI_AVEncMPVDefaultBPictureCount>(), 0);
                _ = config->PlaceEncodingParameters(stream, parameters);
                parameters->Release();
            }
            config->Release();
        }
        IMFMediaType* input = VideoType(MFVideoFormat_NV12, spec);
        try
        {
            Check(writer->SetInputMediaType(stream, input, null), "SetInputMediaType(NV12)");
        }
        finally
        {
            input->Release();
        }
        uint sound = spec.Sound ? AddSound(writer) : uint.MaxValue;
        Check(writer->BeginWriting(), "BeginWriting");
        int size = spec.Width * spec.Height * 3 / 2;
        long step = 10_000_000L / spec.Fps;
        for (int frame = 0; frame < spec.Frames; frame++)
        {
            IMFMediaBuffer* buffer = null;
            Check(MFCreateMemoryBuffer((uint)size, &buffer), "MFCreateMemoryBuffer");
            IMFSample* sample = null;
            try
            {
                byte* data;
                uint max, current;
                Check(buffer->Lock(&data, &max, &current), "Lock");
                Picture(spec, frame, new Span<byte>(data, size));
                _ = buffer->Unlock();
                _ = buffer->SetCurrentLength((uint)size);
                Check(MFCreateSample(&sample), "MFCreateSample");
                _ = sample->AddBuffer(buffer);
                _ = sample->SetSampleTime(frame * step);
                _ = sample->SetSampleDuration(step);
                Check(writer->WriteSample(stream, sample), "WriteSample");
            }
            finally
            {
                if (sample != null)
                {
                    sample->Release();
                }
                buffer->Release();
            }
            if (sound != uint.MaxValue)
            {
                WriteSilence(writer, sound, frame * step, step);
            }
        }
        Check(writer->Finalize(), "Finalize");
    }

    private static IMFMediaType* SoundType(Guid subtype, uint bytesPerSecond, uint blockAlign)
    {
        IMFMediaType* type = null;
        Check(MFCreateMediaType(&type), "MFCreateMediaType");
        Guid major = MFMediaType_Audio;
        _ = type->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &major);
        _ = type->SetGUID(Ref(in MF_MT_SUBTYPE), &subtype);
        _ = type->SetUINT32(Ref(in MF_MT_AUDIO_BITS_PER_SAMPLE), 16);
        _ = type->SetUINT32(Ref(in MF_MT_AUDIO_SAMPLES_PER_SECOND), SoundRate);
        _ = type->SetUINT32(Ref(in MF_MT_AUDIO_NUM_CHANNELS), 1);
        _ = type->SetUINT32(Ref(in MF_MT_AUDIO_AVG_BYTES_PER_SECOND), bytesPerSecond);
        _ = type->SetUINT32(Ref(in MF_MT_AUDIO_BLOCK_ALIGNMENT), blockAlign);
        return type;
    }

    // An AAC stream fed with 16-bit PCM.
    private static uint AddSound(IMFSinkWriter* writer)
    {
        uint stream;
        IMFMediaType* output = SoundType(MFAudioFormat_AAC, 12_000, 1);
        try
        {
            _ = output->SetUINT32(Ref(in MF_MT_AAC_PAYLOAD_TYPE), 0);
            _ = output->SetUINT32(Ref(in MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION), 0x29);
            Check(writer->AddStream(output, &stream), "AddStream(sound)");
        }
        finally
        {
            output->Release();
        }
        IMFMediaType* input = SoundType(MFAudioFormat_PCM, SoundRate * 2, 2);
        try
        {
            Check(writer->SetInputMediaType(stream, input, null), "SetInputMediaType(PCM)");
        }
        finally
        {
            input->Release();
        }
        return stream;
    }

    private static void WriteSilence(IMFSinkWriter* writer, uint stream, long time, long duration)
    {
        uint bytes = (uint)(SoundRate * duration / 10_000_000L) * 2;
        IMFMediaBuffer* buffer = null;
        IMFSample* sample = null;
        Check(MFCreateMemoryBuffer(bytes, &buffer), "MFCreateMemoryBuffer");
        try
        {
            byte* data;
            uint max, current;
            Check(buffer->Lock(&data, &max, &current), "Lock");
            new Span<byte>(data, (int)bytes).Clear();
            _ = buffer->Unlock();
            _ = buffer->SetCurrentLength(bytes);
            Check(MFCreateSample(&sample), "MFCreateSample");
            _ = sample->AddBuffer(buffer);
            _ = sample->SetSampleTime(time);
            _ = sample->SetSampleDuration(duration);
            Check(writer->WriteSample(stream, sample), "WriteSample(sound)");
        }
        finally
        {
            if (sample != null)
            {
                sample->Release();
            }
            buffer->Release();
        }
    }

    private static IMFMediaType* VideoType(Guid subtype, Spec spec)
    {
        IMFMediaType* type = null;
        Check(MFCreateMediaType(&type), "MFCreateMediaType");
        Guid major = MFMediaType_Video;
        _ = type->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &major);
        _ = type->SetGUID(Ref(in MF_MT_SUBTYPE), &subtype);
        _ = type->SetUINT32(Ref(in MF_MT_INTERLACE_MODE), (uint)MFVideoInterlaceMode.MFVideoInterlace_Progressive);
        _ = MFSetAttributeSize((IMFAttributes*)type, Ref(in MF_MT_FRAME_SIZE), (uint)spec.Width, (uint)spec.Height);
        _ = MFSetAttributeRatio((IMFAttributes*)type, Ref(in MF_MT_FRAME_RATE), (uint)spec.Fps, 1);
        _ = MFSetAttributeRatio((IMFAttributes*)type, Ref(in MF_MT_PIXEL_ASPECT_RATIO), 1, 1);
        return type;
    }

    /// <summary>BT.601 video-range Y, Cb, Cr of an RGB color.</summary>
    public static (byte Y, byte Cb, byte Cr) YCbCr((int R, int G, int B) c)
    {
        double r = c.R / 255.0, g = c.G / 255.0, b = c.B / 255.0;
        static byte Clamp(double v) => (byte)Math.Min(Math.Max(Math.Round(v), 0), 255);
        return (
            Clamp(16 + 65.481 * r + 128.553 * g + 24.966 * b),
            Clamp(128 - 37.797 * r - 74.203 * g + 112.0 * b),
            Clamp(128 + 112.0 * r - 93.786 * g - 18.214 * b));
    }

    // One NV12 picture: the luma plane, then the interleaved chroma plane.
    private static void Picture(Spec spec, int frame, Span<byte> nv12)
    {
        int w = spec.Width, h = spec.Height, strip = h * 3 / 4, block = Math.Max(w / Bits, 1);
        (byte y, byte cb, byte cr) = YCbCr(Palette[spec.Color % Palette.Count]);
        for (int row = 0; row < h; row++)
        {
            Span<byte> line = nv12.Slice(row * w, w);
            if (row < strip)
            {
                line.Fill(y);
                continue;
            }
            for (int x = 0; x < w; x++)
            {
                int i = Math.Min(x / block, Bits - 1);
                line[x] = (frame >> (Bits - 1 - i) & 1) == 1 ? (byte)235 : (byte)16;
            }
        }
        for (int row = 0; row < h / 2; row++)
        {
            Span<byte> line = nv12.Slice(w * h + row * w, w);
            bool top = row * 2 < strip;
            for (int x = 0; x < w; x += 2)
            {
                line[x] = top ? cb : (byte)128;
                line[x + 1] = top ? cr : (byte)128;
            }
        }
    }

    /// <summary>
    /// What a rendered picture of a synthetic recording says, read from <paramref name="picture"/> (where the picture is drawn,
    /// in the image's pixels): the camera's color (an index into <see cref="Palette"/>, null when none is within
    /// <paramref name="tolerance"/> per channel) and the frame number.
    /// </summary>
    public static (int? Color, int Frame) Read(RenderedImage image, Rect picture, int tolerance = 40)
    {
        (int R, int G, int B) Pixel(double fx, double fy)
        {
            int x = Math.Min(Math.Max((int)(picture.X + picture.Width * fx), 0), image.Width - 1);
            int y = Math.Min(Math.Max((int)(picture.Y + picture.Height * fy), 0), image.Height - 1);
            return image.Rgb(x, y);
        }
        (int r, int g, int b) = Pixel(0.5, 0.375);
        int? color = null;
        for (int i = 0; i < Palette.Count; i++)
        {
            if (Math.Abs(Palette[i].R - r) <= tolerance && Math.Abs(Palette[i].G - g) <= tolerance && Math.Abs(Palette[i].B - b) <= tolerance)
            {
                color = i;
                break;
            }
        }
        int frame = 0;
        for (int i = 0; i < Bits; i++)
        {
            (int pr, int pg, int pb) = Pixel((i + 0.5) / Bits, 0.875);
            frame = frame << 1 | ((pr + pg + pb) / 3 > 128 ? 1 : 0);
        }
        return (color, frame);
    }

    private static void Check(HRESULT hr, string what)
    {
        if ((int)hr < 0)
        {
            throw new VideoException(what, (int)hr);
        }
    }

    private static Guid* Ref(in Guid g) => (Guid*)Unsafe.AsPointer(ref Unsafe.AsRef(in g));
}
