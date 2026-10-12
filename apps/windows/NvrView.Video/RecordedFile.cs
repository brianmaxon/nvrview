// A finished recording (an MP4 a download wrote) as a picture source for the
// follow cut and grids (docs/UX.md "Recordings", "Follow and Grid"): its video
// track decoded by Media Foundation through an IMFSourceReader (on the GPU
// through the shared device's DXGI manager where it can, else on the CPU) into
// a TilePicture, positioned by file time. Seeking is frame accurate: the reader
// starts at the keyframe at or before the position (read from the file's
// sample tables, Mp4Index) and decodes forward to the picture shown at that
// position; moving forward within a group of pictures only decodes on. With
// keyframesOnly (fast speeds, dragging) only the keyframe is decoded.
//
// Positions are milliseconds on the file's presentation timeline, the same one
// the plan's source_in_ms addresses.
//
// Want decodes on a pool thread, coalescing requests (the newest wins, so
// nothing queues up behind a slow decode); Show decodes on the caller's thread,
// for the self-test and export. Samples are held only as long as the decoder
// can spare them: the picture being chosen and the one read past it.
//
// The picture belongs to the shared GPU device the file was opened on; when
// another window replaces the device (a GPU loss) the reader and the picture
// are rebuilt on the new one at the next request.

using System.Runtime.CompilerServices;
using NvrView.Interop;
using NvrView.Shell;
using TerraFX.Interop.DirectX;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.MFVideoFormat;

namespace NvrView.Video;

/// <summary>A recorded file's video as a picture source (see the file comment).</summary>
public sealed unsafe class RecordedFile : IDisposable
{
    private const uint FirstVideoStream = 0xFFFFFFFC;
    private const uint AllStreams = 0xFFFFFFFE;
    private const uint EndOfStream = 0x2;

    private readonly Func<GpuDevice> _gpuOf;
    private readonly bool _allowHardware;
    private readonly Lock _seekLock = new();
    private readonly Lock _wantLock = new();

    // The request waiting (newest wins), and whether a worker runs.
    private (long Ms, bool KeyframesOnly)? _want;
    private bool _busy;
    private volatile bool _closed;

    // Reader state, under _seekLock.
    private GpuDevice? _gpu;
    private TilePicture? _picture;
    private IMFSourceReader* _reader;
    private long? _shownMs;
    private Held? _next;
    private bool _readerEnded;
    private bool _fresh;
    private long _readerStart;
    private uint _width, _height;
    private int _defaultStride;
    private bool _d3d;
    private int _readers, _decoded;

    // A sample read from the reader and its time.
    private struct Held
    {
        public IMFSample* Sample;
        public long Ms;
    }

    private RecordedFile(string path, Func<GpuDevice> gpuOf, bool allowHardware, Mp4VideoInfo info)
    {
        Path = path;
        _gpuOf = gpuOf;
        _allowHardware = allowHardware;
        Info = info;
    }

    /// <summary>The file.</summary>
    public string Path { get; }

    /// <summary>What the file holds.</summary>
    public Mp4VideoInfo Info { get; }

    /// <summary>Where the pictures are decoded (known after the first one).</summary>
    public DecoderKind Kind { get; private set; }

    /// <summary>The matrix the file's pictures are drawn with.</summary>
    public ColorMatrix Color { get; private set; }

    /// <summary>Readers started and pictures decoded so far (the self-test counts seeks with it).</summary>
    public (int Readers, int Decoded) Stats
    {
        get
        {
            lock (_seekLock)
            {
                return (_readers, _decoded);
            }
        }
    }

    /// <summary>
    /// The picture held, for the renderer: use it only under the device lock. It is replaced when the device changes.
    /// </summary>
    public TilePicture? Picture => Volatile.Read(ref _picture);

    /// <summary>The file position of the picture held, null before the first.</summary>
    public long? ShownPositionMs
    {
        get
        {
            lock (_seekLock)
            {
                return _picture is { HasPicture: true } ? _shownMs : null;
            }
        }
    }

    /// <summary>
    /// Opens a file: reads its description and keyframes and starts a reader. Returns null with the reason in
    /// <paramref name="failure"/> when it holds no video or cannot be read.
    /// </summary>
    public static RecordedFile? Open(string path, Func<GpuDevice> gpuOf, bool allowHardware, out string? failure)
    {
        failure = null;
        if (!File.Exists(path))
        {
            failure = "the file is missing";
            return null;
        }
        GpuDevice.StartMediaFoundation();
        Mp4VideoInfo info = Mp4Index.Read(path) ?? new Mp4VideoInfo(long.MaxValue / 4, 0, 0, "", []);
        var file = new RecordedFile(path, gpuOf, allowHardware, info);
        lock (file._seekLock)
        {
            try
            {
                file.Restart(0);
            }
            catch (VideoException e)
            {
                failure = e.Message;
                file.Dispose();
                return null;
            }
            if (file._reader == null)
            {
                failure = "no video could be read";
                file.Dispose();
                return null;
            }
        }
        return file;
    }

    // ---- positioning ----

    /// <summary>Shows the picture at <paramref name="ms"/>, decoded on a pool thread; a newer request replaces one not yet started.</summary>
    public void Want(long ms, bool keyframesOnly = false)
    {
        if (_closed)
        {
            return;
        }
        bool start;
        lock (_wantLock)
        {
            _want = (ms, keyframesOnly);
            start = !_busy;
            _busy = true;
        }
        if (start)
        {
            ThreadPool.UnsafeQueueUserWorkItem(_ => Drain(), null);
        }
    }

    /// <summary>Shows the picture at <paramref name="ms"/> before returning.</summary>
    public void Show(long ms, bool keyframesOnly = false)
    {
        if (_closed)
        {
            return;
        }
        lock (_seekLock)
        {
            Seek(ms, keyframesOnly);
        }
    }

    private void Drain()
    {
        while (true)
        {
            (long Ms, bool KeyframesOnly) target;
            lock (_wantLock)
            {
                if (_want is not { } t || _closed)
                {
                    _want = null;
                    _busy = false;
                    return;
                }
                target = t;
                _want = null;
            }
            try
            {
                lock (_seekLock)
                {
                    if (!_closed)
                    {
                        Seek(target.Ms, target.KeyframesOnly);
                    }
                }
            }
            catch (VideoException e)
            {
                AppLogHook.Report($"recorded file {System.IO.Path.GetFileName(Path)}: {e.Message}");
            }
        }
    }

    // Under _seekLock.
    private void Seek(long requested, bool keyframesOnly)
    {
        GpuDevice gpu = _gpuOf();
        if (!ReferenceEquals(gpu, _gpu))
        {
            Rebuild(gpu);
        }
        long ms = Math.Max(Math.Min(requested, Math.Max(Info.DurationMs - 1, 0)), 0);
        long k = Info.KeyframeAtOrBefore(ms);
        if (keyframesOnly)
        {
            if (_shownMs == k && _picture is { HasPicture: true })
            {
                return;
            }
            Restart(k);
            if (Read() is { } first)
            {
                Publish(first);
            }
            return;
        }
        // Restart when going back, before the first picture, or when a keyframe lies between the picture shown and the
        // target (decoding from there is no more work than decoding on).
        bool decodeOn = _reader != null && ((_shownMs is { } shown && shown <= ms && k <= shown) || (_shownMs is null && _fresh && _readerStart == k));
        if (!decodeOn)
        {
            Restart(k);
        }
        // The last picture at or before the target; right after a restart the first picture is taken even if it is
        // later (better than black).
        Held? best = null;
        while (true)
        {
            Held f;
            if (_next is { } pending)
            {
                f = pending;
                _next = null;
            }
            else if (Read() is { } read)
            {
                f = read;
            }
            else
            {
                break;
            }
            if (f.Ms <= ms || (best is null && _shownMs is null))
            {
                if (best is { } old)
                {
                    old.Sample->Release();
                }
                best = f;
                if (f.Ms >= ms)
                {
                    break;
                }
            }
            else
            {
                _next = f;
                break;
            }
        }
        if (best is { } chosen)
        {
            Publish(chosen);
        }
    }

    // Under _seekLock: starts a reader at keyframe k on the current device.
    private void Restart(long k)
    {
        ReleaseReader();
        _shownMs = null;
        _readerEnded = false;
        _fresh = true;
        _readerStart = k;
        GpuDevice gpu = _gpuOf();
        if (!ReferenceEquals(gpu, _gpu))
        {
            _gpu = gpu;
            NewPicture(gpu);
        }
        Com.EnsureMta();
        IMFAttributes* attributes = null;
        Check(MFCreateAttributes(&attributes, 4), "MFCreateAttributes");
        try
        {
            _d3d = _allowHardware && gpu.Manager != null;
            if (_d3d)
            {
                _ = attributes->SetUnknown(Ref(in MF_SOURCE_READER_D3D_MANAGER), (IUnknown*)gpu.Manager);
                _ = attributes->SetUINT32(Ref(in MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS), 1);
            }
            _ = attributes->SetUINT32(Ref(in MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING), 1);
            IMFSourceReader* reader = null;
            fixed (char* url = Path)
            {
                Check(MFCreateSourceReaderFromURL(url, attributes, &reader), "MFCreateSourceReaderFromURL");
            }
            _reader = reader;
        }
        finally
        {
            attributes->Release();
        }
        try
        {
            _ = _reader->SetStreamSelection(AllStreams, BOOL.FALSE);
            Check(_reader->SetStreamSelection(FirstVideoStream, BOOL.TRUE), "SetStreamSelection(video)");
            IMFMediaType* type = null;
            Check(MFCreateMediaType(&type), "MFCreateMediaType");
            try
            {
                Guid major = MFMediaType_Video, nv12 = MFVideoFormat_NV12;
                _ = type->SetGUID(Ref(in MF_MT_MAJOR_TYPE), &major);
                _ = type->SetGUID(Ref(in MF_MT_SUBTYPE), &nv12);
                Check(_reader->SetCurrentMediaType(FirstVideoStream, null, type), "SetCurrentMediaType(NV12)");
            }
            finally
            {
                type->Release();
            }
            ReadOutputType();
            if (k > 0)
            {
                PROPVARIANT position = default;
                position.vt = (ushort)VARENUM.VT_I8;
                position.hVal.QuadPart = k * 10_000;
                Guid none = default;
                Check(_reader->SetCurrentPosition(&none, &position), "SetCurrentPosition");
            }
            _readers++;
        }
        catch
        {
            ReleaseReader();
            throw;
        }
    }

    // The decoder's output size, stride, and color description.
    private void ReadOutputType()
    {
        IMFMediaType* current = null;
        Check(_reader->GetCurrentMediaType(FirstVideoStream, &current), "GetCurrentMediaType");
        try
        {
            uint w = 0, h = 0, stride = 0, matrix = 0, range = 0;
            _ = MFGetAttributeSize((IMFAttributes*)current, Ref(in MF_MT_FRAME_SIZE), &w, &h);
            _width = w;
            _height = h;
            _defaultStride = (int)current->GetUINT32(Ref(in MF_MT_DEFAULT_STRIDE), &stride) >= 0 ? (int)stride : (int)w;
            bool hasMatrix = (int)current->GetUINT32(Ref(in MF_MT_YUV_MATRIX), &matrix) >= 0 && matrix != 0;
            bool hasRange = (int)current->GetUINT32(Ref(in MF_MT_VIDEO_NOMINAL_RANGE), &range) >= 0 && range != 0;
            // RENDERING.md's default without a color description: BT.709 at 720 lines and above, BT.601 below.
            // MFVideoTransferMatrix_BT709 = 1, BT601 = 2; MFNominalRange_0_255 = 1.
            bool bt709 = hasMatrix ? matrix == 1 : h >= 720;
            Color = ColorMatrix.Of(bt709, hasRange && range == 1);
            if (_picture is { } p)
            {
                using (_gpu!.Lock())
                {
                    p.Visible = new NvRect { X = 0, Y = 0, Width = w, Height = h };
                    p.Color = Color;
                }
            }
        }
        finally
        {
            current->Release();
        }
    }

    // Reads the next sample of the video stream; null at the end.
    private Held? Read()
    {
        if (_reader == null || _readerEnded)
        {
            return null;
        }
        while (true)
        {
            uint actual = 0, flags = 0;
            long time = 0;
            IMFSample* sample = null;
            int hr = (int)_reader->ReadSample(FirstVideoStream, 0, &actual, &flags, &time, &sample);
            if (hr < 0)
            {
                _readerEnded = true;
                throw new VideoException("ReadSample", hr);
            }
            if (sample != null)
            {
                _decoded++;
                _fresh = false;
                return new Held { Sample = sample, Ms = (time + 5_000) / 10_000 };
            }
            if ((flags & EndOfStream) != 0)
            {
                _readerEnded = true;
                return null;
            }
            if ((flags & 0x20) != 0)
            {
                // The output type changed (a new size): read it again.
                ReadOutputType();
            }
        }
    }

    // Puts the sample into the picture and releases it.
    private void Publish(Held held)
    {
        try
        {
            if (_picture is not { } picture || _gpu is not { } gpu)
            {
                return;
            }
            IMFMediaBuffer* buffer = null;
            if ((int)held.Sample->GetBufferByIndex(0, &buffer) < 0)
            {
                return;
            }
            try
            {
                long pts = held.Ms * 1000;
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
                        return;
                    }
                    if (Kind == DecoderKind.Unknown)
                    {
                        D3D11_TEXTURE2D_DESC desc;
                        texture->GetDesc(&desc);
                        Kind = (desc.BindFlags & (uint)D3D11_BIND_FLAG.D3D11_BIND_DECODER) != 0
                            ? DecoderKind.Hardware : DecoderKind.SoftwareTexture;
                    }
                    using (gpu.Lock())
                    {
                        picture.CopyFrom(texture, subresource, _width, _height, pts);
                    }
                    texture->Release();
                    _shownMs = held.Ms;
                    return;
                }
                if (Upload(buffer, picture, gpu, pts))
                {
                    Kind = DecoderKind.Software;
                    _shownMs = held.Ms;
                }
            }
            finally
            {
                buffer->Release();
            }
        }
        finally
        {
            held.Sample->Release();
        }
    }

    private bool Upload(IMFMediaBuffer* buffer, TilePicture picture, GpuDevice gpu, long pts)
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
            using (gpu.Lock())
            {
                picture.Upload(data, pitch, pitch * (int)_height, _width, _height, pts);
            }
            _ = two->Unlock2D();
            two->Release();
            return true;
        }
        uint max, length;
        if ((int)buffer->Lock(&data, &max, &length) < 0)
        {
            return false;
        }
        pitch = _defaultStride > 0 ? _defaultStride : (int)_width;
        using (gpu.Lock())
        {
            picture.Upload(data, pitch, pitch * (int)_height, _width, _height, pts);
        }
        _ = buffer->Unlock();
        return true;
    }

    // ---- the device ----

    // Under _seekLock: a new device replaced the one the picture and reader were made on.
    private void Rebuild(GpuDevice gpu)
    {
        ReleaseReader();
        _shownMs = null;
        TilePicture? old = _picture;
        Volatile.Write(ref _picture, null);
        if (old != null)
        {
            // The old device is gone or going: its textures are released without its lock.
            old.Dispose();
        }
        _gpu = gpu;
        NewPicture(gpu);
    }

    private void NewPicture(GpuDevice gpu)
    {
        var picture = new TilePicture(gpu) { Color = Color, Visible = new NvRect { X = 0, Y = 0, Width = _width, Height = _height } };
        TilePicture? old = Interlocked.Exchange(ref _picture, picture);
        if (old != null)
        {
            using (gpu.Lock())
            {
                old.Dispose();
            }
        }
    }

    private void ReleaseReader()
    {
        if (_next is { } pending)
        {
            pending.Sample->Release();
            _next = null;
        }
        if (_reader != null)
        {
            _reader->Release();
            _reader = null;
        }
    }

    /// <summary>Lets go of the reader and the picture (the tile draws black).</summary>
    public void Dispose()
    {
        if (_closed)
        {
            return;
        }
        _closed = true;
        lock (_seekLock)
        {
            ReleaseReader();
            TilePicture? picture = Interlocked.Exchange(ref _picture, null);
            if (picture != null)
            {
                if (_gpu is { IsDisposed: false } gpu)
                {
                    using (gpu.Lock())
                    {
                        picture.Dispose();
                    }
                }
                else
                {
                    // The device it was made on is gone: its textures are released without its lock.
                    picture.Dispose();
                }
            }
        }
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

/// <summary>Where this library reports what it cannot throw (the app points it at its log).</summary>
public static class AppLogHook
{
    /// <summary>Called with a line to log; null drops it.</summary>
    public static Action<string>? Sink { get; set; }

    internal static void Report(string line) => Sink?.Invoke(line);
}
