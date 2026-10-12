// Export writers on Media Foundation (docs/UX.md "Recordings", "Export"): a
// downloaded camera file cut to a span, either by copying its compressed
// samples (a keyframe copy: instant, lossless) or by decoding and encoding
// it again (exact: it starts at the picture asked for), the picture at a
// time of a file, and an MP4 written from frames an app draws (a follow cut
// or grid). Platform work only; what to cut and where is decided elsewhere
// (NvrView.Shell). Every writer writes the file it is given and leaves
// finishing it (a hidden name, the move into place) to the caller, so a
// canceled or failed export is a file to delete.

using System.Runtime.CompilerServices;
using NvrView.Interop;
using NvrView.Tiles;
using TerraFX.Interop.DirectX;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.MFTranscodeContainerType;
using static TerraFX.Interop.Windows.MFT;
using static TerraFX.Interop.Windows.MFVideoFormat;
using static TerraFX.Interop.Windows.Windows;

namespace NvrView.Video;

/// <summary>How an export ended.</summary>
public enum MfExportStatus
{
    /// <summary>The file is whole.</summary>
    Done,

    /// <summary>Stopped on request; the file is partial.</summary>
    Canceled,

    /// <summary>It could not be made; the file is partial.</summary>
    Failed,
}

/// <summary>The outcome of one export; <see cref="Reason"/> is for the user when it failed.</summary>
public readonly record struct MfExportResult(MfExportStatus Status, string? Reason = null, int Width = 0, int Height = 0, int Frames = 0)
{
    /// <summary>A failure with the words for the user.</summary>
    public static MfExportResult Failed(string reason) => new(MfExportStatus.Failed, reason);
}

/// <summary>The writers (see the file comment).</summary>
public static unsafe class MfExport
{
    private const uint FirstVideo = 0xFFFFFFFC, FirstAudio = 0xFFFFFFFD, AllStreams = 0xFFFFFFFE;
    private const uint EndOfStream = 0x2, StreamTick = 0x100, ReadError = 0x1;
    private const long HundredNs = 10_000;

    private static readonly object s_gate = new();

    private static Guid* Ref(in Guid g) => (Guid*)Unsafe.AsPointer(ref Unsafe.AsRef(in g));

    private static void Start()
    {
        lock (s_gate)
        {
            GpuDevice.StartMediaFoundation();
        }
        Com.EnsureMta();
    }

    private static void Ok(HRESULT hr, string what)
    {
        if ((int)hr < 0)
        {
            throw new VideoException(what, (int)hr);
        }
    }

    private static void Release<T>(ref T* p) where T : unmanaged, IUnknown.Interface
    {
        if (p != null)
        {
            _ = p->Release();
            p = null;
        }
    }

    private static string Why(VideoException e) => $"{e.Message}";

    // ---- the streams of a file ----

    private sealed class Stream
    {
        public uint Index;
        public bool Video;
        public uint Sink;
        public IMFSample* Pending;
        public long PendingTime;
        public bool Ended;
    }

    // Finds the first video and audio stream of the reader (native types).
    private static List<Stream> Streams(IMFSourceReader* reader, out IMFMediaType* videoType)
    {
        var list = new List<Stream>();
        videoType = null;
        bool haveVideo = false, haveAudio = false;
        for (uint i = 0; i < 32 && !(haveVideo && haveAudio); i++)
        {
            IMFMediaType* type = null;
            if ((int)reader->GetNativeMediaType(i, 0, &type) < 0)
            {
                break;
            }
            Guid major;
            if ((int)type->GetGUID(Ref(in MF_MT_MAJOR_TYPE), &major) >= 0)
            {
                if (major == MFMediaType_Video && !haveVideo)
                {
                    haveVideo = true;
                    list.Add(new Stream { Index = i, Video = true });
                    videoType = type;
                    continue;
                }
                if (major == MFMediaType_Audio && !haveAudio)
                {
                    haveAudio = true;
                    list.Add(new Stream { Index = i });
                }
            }
            type->Release();
        }
        return list;
    }

    private static IMFSourceReader* OpenReader(string path, bool processing)
    {
        IMFAttributes* attributes = null;
        IMFSourceReader* reader = null;
        try
        {
            Ok(MFCreateAttributes(&attributes, 2), "MFCreateAttributes");
            _ = attributes->SetUINT32(Ref(in MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING), processing ? 1u : 0u);
            // Software decoding and encoding are the same everywhere; an export is not in a hurry.
            _ = attributes->SetUINT32(Ref(in MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS), 0);
            fixed (char* p = path)
            {
                Ok(MFCreateSourceReaderFromURL(p, attributes, &reader), "MFCreateSourceReaderFromURL");
            }
            return reader;
        }
        finally
        {
            Release(ref attributes);
        }
    }

    private static IMFSinkWriter* OpenWriter(string path)
    {
        IMFAttributes* attributes = null;
        IMFSinkWriter* writer = null;
        try
        {
            Ok(MFCreateAttributes(&attributes, 3), "MFCreateAttributes");
            Guid mp4 = MFTranscodeContainerType_MPEG4;
            _ = attributes->SetGUID(Ref(in MF_TRANSCODE_CONTAINERTYPE), &mp4);
            _ = attributes->SetUINT32(Ref(in MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS), 0);
            _ = attributes->SetUINT32(Ref(in MF_SINK_WRITER_DISABLE_THROTTLING), 1);
            fixed (char* p = path)
            {
                Ok(MFCreateSinkWriterFromURL(p, null, attributes, &writer), "MFCreateSinkWriterFromURL");
            }
            return writer;
        }
        finally
        {
            Release(ref attributes);
        }
    }

    private static void Seek(IMFSourceReader* reader, long ms)
    {
        PROPVARIANT position = default;
        position.vt = (ushort)VARENUM.VT_I8;
        position.hVal.QuadPart = ms * HundredNs;
        Guid none = Guid.Empty;
        Ok(reader->SetCurrentPosition(&none, &position), "SetCurrentPosition");
    }

    // Reads the next sample of one stream; null at its end.
    private static void Pull(IMFSourceReader* reader, Stream s)
    {
        s.Pending = null;
        while (true)
        {
            uint flags;
            long time;
            IMFSample* sample = null;
            uint actual;
            Ok(reader->ReadSample(s.Index, 0, &actual, &flags, &time, &sample), "ReadSample");
            if ((flags & (EndOfStream | ReadError)) != 0)
            {
                s.Ended = true;
                if (sample != null)
                {
                    _ = sample->Release();
                }
                return;
            }
            if (sample == null || (flags & StreamTick) != 0)
            {
                if (sample != null)
                {
                    _ = sample->Release();
                }
                continue;
            }
            s.Pending = sample;
            s.PendingTime = time;
            return;
        }
    }

    // ---- trim ----

    /// <summary>
    /// Cuts the file to <paramref name="inMs"/> to <paramref name="outMs"/> (milliseconds from its first picture) into
    /// <paramref name="partialPath"/>. A keyframe copy starts at the keyframe the plan chose (<paramref name="inMs"/>
    /// itself); exact decodes from there and re-encodes (H.264) so the first picture is the one asked for. Sound is
    /// carried as it is (AAC).
    /// </summary>
    public static MfExportResult Cut(string sourcePath, long inMs, long outMs, bool exact, string partialPath, Func<bool> canceled, Action<double> progress)
    {
        Start();
        IMFSourceReader* reader = null;
        IMFSinkWriter* writer = null;
        IMFMediaType* videoType = null;
        List<Stream> streams = [];
        int width = 0, height = 0, frames = 0;
        try
        {
            reader = OpenReader(sourcePath, processing: exact);
            streams = Streams(reader, out videoType);
            if (videoType == null || streams.FirstOrDefault(s => s.Video) is not { } video)
            {
                return MfExportResult.Failed("The downloaded file has no video");
            }
            _ = reader->SetStreamSelection(AllStreams, false);
            foreach (Stream s in streams)
            {
                Ok(reader->SetStreamSelection(s.Index, true), "SetStreamSelection");
            }
            uint w = 0, h = 0, rateN = 30, rateD = 1;
            _ = MFGetAttributeSize((IMFAttributes*)videoType, Ref(in MF_MT_FRAME_SIZE), &w, &h);
            if ((int)MFGetAttributeRatio((IMFAttributes*)videoType, Ref(in MF_MT_FRAME_RATE), &rateN, &rateD) < 0 || rateN == 0 || rateD == 0)
            {
                rateN = 30;
                rateD = 1;
            }
            width = (int)w;
            height = (int)h;
            writer = OpenWriter(partialPath);
            foreach (Stream s in streams)
            {
                IMFMediaType* native = null;
                IMFMediaType* output = null;
                IMFMediaType* input = null;
                try
                {
                    Ok(reader->GetNativeMediaType(s.Index, 0, &native), "GetNativeMediaType");
                    if (s.Video && exact)
                    {
                        output = VideoType(MFVideoFormat_H264, w, h, rateN, rateD, bitrate: Bitrate(w, h, rateN, rateD));
                        input = VideoType(MFVideoFormat_NV12, w, h, rateN, rateD, bitrate: 0);
                        Ok(reader->SetCurrentMediaType(s.Index, null, input), "SetCurrentMediaType");
                        uint index;
                        Ok(writer->AddStream(output, &index), "AddStream");
                        s.Sink = index;
                        Ok(writer->SetInputMediaType(s.Sink, input, null), "SetInputMediaType");
                    }
                    else
                    {
                        uint index;
                        Ok(writer->AddStream(native, &index), "AddStream");
                        s.Sink = index;
                        Ok(writer->SetInputMediaType(s.Sink, native, null), "SetInputMediaType");
                    }
                }
                finally
                {
                    Release(ref native);
                    Release(ref output);
                    Release(ref input);
                }
            }
            Ok(writer->BeginWriting(), "BeginWriting");
            Seek(reader, inMs);
            long endTime = outMs * HundredNs, inTime = inMs * HundredNs;
            double span = Math.Max(outMs - inMs, 1) * (double)HundredNs;
            foreach (Stream s in streams)
            {
                Pull(reader, s);
            }
            // The output's time zero is the first picture it holds (the keyframe for a copy, the first picture at or
            // after the in point when exact); sound before that is left out.
            long origin = -1;
            while (true)
            {
                if (canceled())
                {
                    return new MfExportResult(MfExportStatus.Canceled);
                }
                Stream? next = null;
                foreach (Stream s in streams)
                {
                    if (s.Pending != null && (next is null || s.PendingTime < next.PendingTime))
                    {
                        next = s;
                    }
                }
                if (next is null)
                {
                    break;
                }
                IMFSample* sample = next.Pending;
                long time = next.PendingTime;
                next.Pending = null;
                try
                {
                    if (time >= endTime)
                    {
                        next.Ended = true;
                        continue;
                    }
                    if (next.Video)
                    {
                        if (exact && time < inTime)
                        {
                            Pull(reader, next);
                            continue;
                        }
                        if (origin < 0)
                        {
                            origin = time;
                        }
                        frames++;
                        progress(Math.Min((time - inTime) / span, 1));
                    }
                    else if (origin < 0 || time < origin)
                    {
                        Pull(reader, next);
                        continue;
                    }
                    _ = sample->SetSampleTime(time - origin);
                    Ok(writer->WriteSample(next.Sink, sample), "WriteSample");
                    Pull(reader, next);
                }
                finally
                {
                    _ = sample->Release();
                }
            }
            if (frames == 0)
            {
                return MfExportResult.Failed("The cut holds no pictures");
            }
            Ok(writer->Finalize(), "Finalize");
            return new MfExportResult(MfExportStatus.Done, null, width, height, frames);
        }
        catch (VideoException e)
        {
            return MfExportResult.Failed($"The cut failed: {Why(e)}");
        }
        finally
        {
            foreach (Stream s in streams)
            {
                if (s.Pending != null)
                {
                    _ = s.Pending->Release();
                }
            }
            Release(ref videoType);
            Release(ref writer);
            Release(ref reader);
        }
    }

    private static uint Bitrate(uint w, uint h, uint rateN, uint rateD)
    {
        double fps = rateD == 0 ? 30 : (double)rateN / rateD;
        return (uint)Math.Max(500_000, w * (double)h * fps * 0.1);
    }

    private static IMFMediaType* VideoType(Guid subtype, uint w, uint h, uint rateN, uint rateD, uint bitrate, int stride = 0)
    {
        IMFMediaType* type = null;
        Ok(MFCreateMediaType(&type), "MFCreateMediaType");
        Guid major = MFMediaType_Video;
        _ = type->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &major);
        _ = type->SetGUID(Ref(in MF_MT_SUBTYPE), &subtype);
        _ = MFSetAttributeSize((IMFAttributes*)type, Ref(in MF_MT_FRAME_SIZE), w, h);
        _ = MFSetAttributeRatio((IMFAttributes*)type, Ref(in MF_MT_FRAME_RATE), rateN, rateD);
        _ = MFSetAttributeRatio((IMFAttributes*)type, Ref(in MF_MT_PIXEL_ASPECT_RATIO), 1, 1);
        _ = type->SetUINT32(Ref(in MF_MT_INTERLACE_MODE), (uint)MFVideoInterlaceMode.MFVideoInterlace_Progressive);
        if (bitrate > 0)
        {
            _ = type->SetUINT32(Ref(in MF_MT_AVG_BITRATE), bitrate);
        }
        if (stride != 0)
        {
            _ = type->SetUINT32(Ref(in MF_MT_DEFAULT_STRIDE), (uint)stride);
        }
        return type;
    }

    // ---- what a file holds ----

    /// <summary>The length (milliseconds) and picture size of an MP4's video; null when it cannot be read.</summary>
    public static (long DurationMs, int Width, int Height)? Probe(string path)
    {
        Start();
        IMFSourceReader* reader = null;
        IMFMediaType* native = null;
        try
        {
            reader = OpenReader(path, processing: false);
            Ok(reader->GetNativeMediaType(FirstVideo, 0, &native), "GetNativeMediaType");
            uint w = 0, h = 0;
            _ = MFGetAttributeSize((IMFAttributes*)native, Ref(in MF_MT_FRAME_SIZE), &w, &h);
            PROPVARIANT duration = default;
            Ok(reader->GetPresentationAttribute(0xFFFFFFFF, Ref(in MF_PD_DURATION), &duration), "GetPresentationAttribute");
            return (duration.uhVal.QuadPart / (ulong)HundredNs > long.MaxValue ? 0 : (long)(duration.uhVal.QuadPart / (ulong)HundredNs), (int)w, (int)h);
        }
        catch (VideoException)
        {
            return null;
        }
        finally
        {
            Release(ref native);
            Release(ref reader);
        }
    }

    // ---- a picture of a file ----

    /// <summary>
    /// The picture of a downloaded file at <paramref name="atMs"/> (milliseconds from its first picture), decoded from
    /// the keyframe at or before; <paramref name="exact"/> decodes on to the picture asked for, otherwise the keyframe
    /// itself is the picture. Null when the file cannot be read.
    /// </summary>
    public static RenderedImage? Picture(string path, long atMs, bool exact)
    {
        Start();
        IMFSourceReader* reader = null;
        IMFMediaType* want = null;
        IMFMediaType* got = null;
        try
        {
            reader = OpenReader(path, processing: true);
            _ = reader->SetStreamSelection(AllStreams, false);
            Ok(reader->SetStreamSelection(FirstVideo, true), "SetStreamSelection");
            IMFMediaType* native = null;
            uint w = 0, h = 0;
            Ok(reader->GetNativeMediaType(FirstVideo, 0, &native), "GetNativeMediaType");
            _ = MFGetAttributeSize((IMFAttributes*)native, Ref(in MF_MT_FRAME_SIZE), &w, &h);
            _ = native->Release();
            want = VideoType(MFVideoFormat_RGB32, w, h, 30, 1, bitrate: 0);
            Ok(reader->SetCurrentMediaType(FirstVideo, null, want), "SetCurrentMediaType");
            Seek(reader, atMs);
            long target = atMs * HundredNs;
            IMFSample* last = null;
            try
            {
                while (true)
                {
                    uint actual, flags;
                    long time;
                    IMFSample* sample = null;
                    Ok(reader->ReadSample(FirstVideo, 0, &actual, &flags, &time, &sample), "ReadSample");
                    if ((flags & (EndOfStream | ReadError)) != 0)
                    {
                        if (sample != null)
                        {
                            _ = sample->Release();
                        }
                        break;
                    }
                    if (sample == null)
                    {
                        continue;
                    }
                    if (last != null)
                    {
                        _ = last->Release();
                    }
                    last = sample;
                    // The first picture is the keyframe; exact goes on until the picture at or after the time asked.
                    if (!exact || time >= target)
                    {
                        break;
                    }
                }
                if (last == null)
                {
                    return null;
                }
                Ok(reader->GetCurrentMediaType(FirstVideo, &got), "GetCurrentMediaType");
                uint cw = 0, ch = 0, stride = 0;
                _ = MFGetAttributeSize((IMFAttributes*)got, Ref(in MF_MT_FRAME_SIZE), &cw, &ch);
                if ((int)got->GetUINT32(Ref(in MF_MT_DEFAULT_STRIDE), &stride) < 0)
                {
                    stride = (uint)(-(int)cw * 4);
                }
                return Copy(last, (int)cw, (int)ch, (int)stride, (int)Math.Min(w, cw), (int)Math.Min(h, ch));
            }
            finally
            {
                if (last != null)
                {
                    _ = last->Release();
                }
            }
        }
        catch (VideoException)
        {
            return null;
        }
        finally
        {
            Release(ref want);
            Release(ref got);
            Release(ref reader);
        }
    }

    // RGB32 rows to a top-down BGRA image; a negative stride means the rows come bottom first. The decoder pads a
    // picture to whole macroblocks (368 rows for 360), at the bottom and right, so the image is cropped to the
    // size the file says (outWidth by outHeight) from the top left.
    private static RenderedImage? Copy(IMFSample* sample, int width, int height, int stride, int outWidth, int outHeight)
    {
        IMFMediaBuffer* buffer = null;
        try
        {
            Ok(sample->ConvertToContiguousBuffer(&buffer), "ConvertToContiguousBuffer");
            byte* data;
            uint max, current;
            Ok(buffer->Lock(&data, &max, &current), "Lock");
            try
            {
                int row = Math.Abs(stride);
                if (width < 1 || height < 1 || row < width * 4 || (long)row * height > current)
                {
                    return null;
                }
                var pixels = new byte[outWidth * outHeight * 4];
                for (int y = 0; y < outHeight; y++)
                {
                    int from = stride < 0 ? height - 1 - y : y;
                    new ReadOnlySpan<byte>(data + (long)from * row, outWidth * 4).CopyTo(pixels.AsSpan(y * outWidth * 4));
                }
                for (int i = 3; i < pixels.Length; i += 4)
                {
                    pixels[i] = 255;
                }
                return new RenderedImage(outWidth, outHeight, pixels);
            }
            finally
            {
                _ = buffer->Unlock();
            }
        }
        finally
        {
            Release(ref buffer);
        }
    }

    // ---- an MP4 from drawn frames ----

    /// <summary>
    /// Writes <paramref name="frameCount"/> frames at <paramref name="fps"/> into <paramref name="partialPath"/> (H.264
    /// or HEVC); <paramref name="draw"/> fills a frame's pixels (BGRA, top row first) and says whether it could.
    /// <paramref name="keyframeInterval"/> frames separate keyframes (0: the encoder's choice). <paramref name="sound"/>
    /// places pieces of the files' sound (AAC, copied as it is) in the output; a piece whose file has no sound, or sound
    /// in another format than the first piece's, is left silent.
    /// </summary>
    public static MfExportResult RenderEdit(
        string partialPath, int width, int height, int fps, int frameCount, bool hevc, int keyframeInterval,
        Func<int, byte[], bool> draw, Func<bool> canceled, Action<double> progress, IReadOnlyList<MfSoundPiece>? sound = null)
    {
        Start();
        IMFSinkWriter* writer = null;
        IMFMediaType* output = null;
        IMFMediaType* input = null;
        IMFSample* sample = null;
        IMFMediaBuffer* buffer = null;
        using var feed = new SoundFeed(sound ?? []);
        try
        {
            writer = OpenWriter(partialPath);
            output = VideoType(hevc ? MFVideoFormat_HEVC : MFVideoFormat_H264, (uint)width, (uint)height, (uint)fps, 1,
                Bitrate((uint)width, (uint)height, (uint)fps, 1));
            if (keyframeInterval > 0)
            {
                _ = output->SetUINT32(Ref(in MF_MT_MAX_KEYFRAME_SPACING), (uint)keyframeInterval);
            }
            // RGB32 with a positive stride is top row first; the writer converts to what the encoder takes.
            input = VideoType(MFVideoFormat_RGB32, (uint)width, (uint)height, (uint)fps, 1, bitrate: 0, stride: width * 4);
            uint sink;
            HRESULT added = writer->AddStream(output, &sink);
            if ((int)added < 0)
            {
                return MfExportResult.Failed(hevc ? "This computer cannot write H.265 video; choose H.264" : $"The video could not be set up (0x{(int)added:X8})");
            }
            Ok(writer->SetInputMediaType(sink, input, null), "SetInputMediaType");
            feed.AddTo(writer);
            Ok(writer->BeginWriting(), "BeginWriting");
            var pixels = new byte[width * height * 4];
            long frameDuration = 10_000_000L / fps;
            long end = frameCount * 10_000_000L / fps;
            for (int i = 0; i < frameCount; i++)
            {
                if (canceled())
                {
                    return new MfExportResult(MfExportStatus.Canceled);
                }
                // The sound up to the end of this frame goes first, so the file interleaves.
                feed.WriteUntil(writer, Math.Min((i + 1) * 10_000_000L / fps, end), end);
                if (!draw(i, pixels))
                {
                    return MfExportResult.Failed("A picture of the video could not be drawn");
                }
                Ok(MFCreateMemoryBuffer((uint)pixels.Length, &buffer), "MFCreateMemoryBuffer");
                byte* data;
                uint max, current;
                Ok(buffer->Lock(&data, &max, &current), "Lock");
                pixels.CopyTo(new Span<byte>(data, pixels.Length));
                _ = buffer->Unlock();
                _ = buffer->SetCurrentLength((uint)pixels.Length);
                Ok(MFCreateSample(&sample), "MFCreateSample");
                _ = sample->AddBuffer(buffer);
                _ = sample->SetSampleTime(i * 10_000_000L / fps);
                _ = sample->SetSampleDuration(frameDuration);
                Ok(writer->WriteSample(sink, sample), "WriteSample");
                Release(ref sample);
                Release(ref buffer);
                progress((i + 1.0) / frameCount);
            }
            feed.WriteUntil(writer, end, end);
            Ok(writer->Finalize(), "Finalize");
            return new MfExportResult(MfExportStatus.Done, null, width, height, frameCount);
        }
        catch (VideoException e)
        {
            return MfExportResult.Failed($"The video could not be written: {Why(e)}");
        }
        finally
        {
            Release(ref sample);
            Release(ref buffer);
            Release(ref input);
            Release(ref output);
            Release(ref writer);
        }
    }

    // Two sound formats are the same when their subtype, rate, channels, and codec configuration (user data) are.
    private static bool SameSound(IMFMediaType* a, IMFMediaType* b)
    {
        Guid sa, sb;
        uint ra = 0, rb = 0, ca = 0, cb = 0;
        if ((int)a->GetGUID(Ref(in MF_MT_SUBTYPE), &sa) < 0 || (int)b->GetGUID(Ref(in MF_MT_SUBTYPE), &sb) < 0 || sa != sb)
        {
            return false;
        }
        _ = a->GetUINT32(Ref(in MF_MT_AUDIO_SAMPLES_PER_SECOND), &ra);
        _ = b->GetUINT32(Ref(in MF_MT_AUDIO_SAMPLES_PER_SECOND), &rb);
        _ = a->GetUINT32(Ref(in MF_MT_AUDIO_NUM_CHANNELS), &ca);
        _ = b->GetUINT32(Ref(in MF_MT_AUDIO_NUM_CHANNELS), &cb);
        return ra == rb && ca == cb && Blob(a, MF_MT_USER_DATA).AsSpan().SequenceEqual(Blob(b, MF_MT_USER_DATA));
    }

    private static byte[] Blob(IMFMediaType* type, in Guid key)
    {
        uint size = 0;
        if ((int)type->GetBlobSize(Ref(in key), &size) < 0 || size == 0)
        {
            return [];
        }
        var data = new byte[size];
        fixed (byte* p = data)
        {
            uint got;
            return (int)type->GetBlob(Ref(in key), p, size, &got) < 0 ? [] : data[..(int)got];
        }
    }

    // The sound of an exported edit: each piece's samples read from its file (compressed, as they are) and moved to
    // the piece's place in the output. Samples that would overlap the previous piece's are dropped, so times only rise.
    private sealed class SoundFeed(IReadOnlyList<MfSoundPiece> pieces) : IDisposable
    {
        private IMFMediaType* _format;
        private IMFSourceReader* _reader;
        private uint _stream;
        private int _next;
        private MfSoundPiece _piece;
        private IMFSample* _pending;
        private long _pendingTime;
        private long _written;
        private uint _sink = uint.MaxValue;

        // Adds the sound stream (the first piece's format) before the writer begins; no stream when no piece has sound.
        public void AddTo(IMFSinkWriter* writer)
        {
            foreach (MfSoundPiece piece in pieces)
            {
                IMFSourceReader* reader = null;
                try
                {
                    reader = OpenReader(piece.File, processing: false);
                    IMFMediaType* native = null;
                    if ((int)reader->GetNativeMediaType(FirstAudio, 0, &native) < 0)
                    {
                        continue;
                    }
                    uint sink;
                    if ((int)writer->AddStream(native, &sink) < 0 || (int)writer->SetInputMediaType(sink, native, null) < 0)
                    {
                        AppLogHook.Report("export: the edit's sound format cannot be written; the export is silent");
                        native->Release();
                        return;
                    }
                    _format = native;
                    _sink = sink;
                    return;
                }
                catch (VideoException)
                {
                }
                finally
                {
                    Release(ref reader);
                }
            }
        }

        // Writes the sound samples that start before `until` (100 ns), none at or after `end`.
        public void WriteUntil(IMFSinkWriter* writer, long until, long end)
        {
            if (_sink == uint.MaxValue)
            {
                return;
            }
            while (Pending() && _pendingTime < until)
            {
                IMFSample* sample = _pending;
                _pending = null;
                try
                {
                    if (_pendingTime < end)
                    {
                        long duration = 0;
                        _ = sample->GetSampleDuration(&duration);
                        _ = sample->SetSampleTime(_pendingTime);
                        Ok(writer->WriteSample(_sink, sample), "WriteSample(sound)");
                        _written = _pendingTime + Math.Max(duration, 1);
                    }
                }
                finally
                {
                    _ = sample->Release();
                }
            }
        }

        // Reads ahead to the next sample to write, opening pieces in turn; false when none is left.
        private bool Pending()
        {
            while (_pending == null)
            {
                if (_reader == null && !OpenNext())
                {
                    return false;
                }
                uint flags, actual;
                long time;
                IMFSample* sample = null;
                if ((int)_reader->ReadSample(_stream, 0, &actual, &flags, &time, &sample) < 0 || (flags & (EndOfStream | ReadError)) != 0)
                {
                    if (sample != null)
                    {
                        _ = sample->Release();
                    }
                    Release(ref _reader);
                    continue;
                }
                if (sample == null || (flags & StreamTick) != 0)
                {
                    if (sample != null)
                    {
                        _ = sample->Release();
                    }
                    continue;
                }
                long duration = 0;
                _ = sample->GetSampleDuration(&duration);
                long from = _piece.FileStartMs * HundredNs, to = _piece.FileEndMs * HundredNs;
                if (time >= to)
                {
                    _ = sample->Release();
                    Release(ref _reader);
                    continue;
                }
                long at = (_piece.AtMs * HundredNs) + (time - from);
                if (time + duration <= from || at < _written || at < 0)
                {
                    _ = sample->Release();
                    continue;
                }
                _pending = sample;
                _pendingTime = at;
            }
            return true;
        }

        // Opens the next piece whose file has sound in the stream's format, at the piece's start.
        private bool OpenNext()
        {
            while (_next < pieces.Count)
            {
                _piece = pieces[_next++];
                if (_piece.FileEndMs <= _piece.FileStartMs)
                {
                    continue;
                }
                IMFMediaType* native = null;
                try
                {
                    _reader = OpenReader(_piece.File, processing: false);
                    List<Stream> streams = Streams(_reader, out IMFMediaType* video);
                    Release(ref video);
                    if (streams.FirstOrDefault(s => !s.Video) is not { } audio
                        || (int)_reader->GetNativeMediaType(audio.Index, 0, &native) < 0 || !SameSound(native, _format))
                    {
                        Release(ref _reader);
                        continue;
                    }
                    _stream = audio.Index;
                    _ = _reader->SetStreamSelection(AllStreams, false);
                    Ok(_reader->SetStreamSelection(_stream, true), "SetStreamSelection(sound)");
                    Seek(_reader, Math.Max(_piece.FileStartMs, 0));
                    return true;
                }
                catch (VideoException)
                {
                    Release(ref _reader);
                }
                finally
                {
                    Release(ref native);
                }
            }
            return false;
        }

        public void Dispose()
        {
            if (_pending != null)
            {
                _ = _pending->Release();
                _pending = null;
            }
            Release(ref _reader);
            Release(ref _format);
        }
    }

    /// <summary>The span (milliseconds) the first sound stream's samples cover, and their count; null when the file has no sound.</summary>
    public static (long StartMs, long EndMs, int Samples)? SoundSpan(string path)
    {
        Start();
        IMFSourceReader* reader = null;
        IMFMediaType* native = null;
        try
        {
            reader = OpenReader(path, processing: false);
            if ((int)reader->GetNativeMediaType(FirstAudio, 0, &native) < 0)
            {
                return null;
            }
            _ = reader->SetStreamSelection(AllStreams, false);
            Ok(reader->SetStreamSelection(FirstAudio, true), "SetStreamSelection");
            long first = -1, last = 0;
            int count = 0;
            while (true)
            {
                uint flags, actual;
                long time;
                IMFSample* sample = null;
                Ok(reader->ReadSample(FirstAudio, 0, &actual, &flags, &time, &sample), "ReadSample");
                if (sample != null)
                {
                    long duration = 0;
                    _ = sample->GetSampleDuration(&duration);
                    _ = sample->Release();
                    first = first < 0 ? time : first;
                    last = Math.Max(last, time + duration);
                    count++;
                }
                if ((flags & (EndOfStream | ReadError)) != 0)
                {
                    break;
                }
            }
            return count == 0 ? (0, 0, 0) : (first / HundredNs, last / HundredNs, count);
        }
        catch (VideoException)
        {
            return null;
        }
        finally
        {
            Release(ref native);
            Release(ref reader);
        }
    }
}

/// <summary>A piece of a file's sound placed in an exported edit.</summary>
/// <param name="File">The MP4 whose first sound stream is read.</param>
/// <param name="FileStartMs">Where in the file the piece starts (milliseconds from its first picture).</param>
/// <param name="FileEndMs">Where it ends.</param>
/// <param name="AtMs">Where it starts in the output, from its first frame.</param>
public readonly record struct MfSoundPiece(string File, long FileStartMs, long FileEndMs, long AtMs);
