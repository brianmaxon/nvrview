// One window's render thread: draws its tiles into the window's swap chain
// once per display refresh (docs/UX.md "Full screen and focus",
// docs/RENDERING.md "The platform renderer"). Every window has its own; they
// share the GPU device and its lock.
//
// - Pacing: waits on the swap chain's frame latency waitable, then lays the
//   tiles out, draws, and presents. It keeps doing so during resizes and
//   focus animations; nothing is allocated per frame in steady state.
// - Resize: the panel's size (from the UI thread) is followed by
//   ResizeBuffers at most every 100 ms while it keeps changing, then once
//   more at the exact size; in between the old buffers are shown stretched
//   (SetMatrixTransform), so presentation never stops.
// - Layout: the window's split tree, laid out by the UI thread into cells
//   (DIPs) whenever it or the size changes (StageLayout); here each picture
//   is fitted into its cell, the focused tile's rectangle animated between
//   its cell and the whole stage, and digital zoom applied as a smaller
//   source rectangle. Each tile's rectangle (DIPs) is published on its
//   StageTile for the chrome and hit testing.
// - Device loss: a Present that reports the device removed or reset (or a
//   simulated loss in the self-test) releases the swap chain and renderer,
//   has CoreHost replace the device once (other windows' render threads see
//   the new device and rebuild their own swap chains), and starts again; the
//   UI thread attaches the new swap chain to the panel.
// - Fisheye: a tile in a dewarped view (StageTile.Fisheye) fills its whole
//   cell and is drawn through the dewarp shader with constants computed
//   here when its view, circle or size changes (docs/RENDERING.md "Fisheye
//   dewarping"); until its camera's circle is found, this thread hands a
//   copy of the luma plane to FisheyeCircles a few times.
// - Overlays: the pictures of the window's call cards (docs/UX.md "Alert
//   insets"), each one more tile of this grid drawn last, on top of
//   everything (also over a focused tile), from the camera's shared tile:
//   no second decoder. The UI thread publishes where each card's
//   transparent hole is (DIPs, like the cells); a device loss brings them
//   back with the rest, since they are part of every frame.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using static TerraFX.Interop.DirectX.DXGI;

namespace NvrView.App;

/// <summary>The focused tile and its animation (immutable; replaced on each change).</summary>
internal sealed class FocusAnimation(StageTile tile, long start, bool entering, float from)
{
    public const double Seconds = 0.25;

    public StageTile Tile { get; } = tile;

    public long Start { get; } = start;

    public bool Entering { get; } = entering;

    public float From { get; } = from;

    /// <summary>Linear progress 0 (grid) .. 1 (focused) at <paramref name="now"/>.</summary>
    public float Progress(long now)
    {
        double t = (now - Start) / (double)Stopwatch.Frequency / Seconds;
        double p = From + (Entering ? t : -t);
        return (float)Math.Clamp(p, 0, 1);
    }

    /// <summary>Eased progress for drawing.</summary>
    public static float Ease(float p) => p * p * (3 - 2 * p);
}

/// <summary>What one window draws: its tiles and their cells in DIPs for a stage of Width x Height (immutable; replaced on each change).</summary>
internal sealed class StageLayout(StageTile[] tiles, Rect[] cells, double width, double height)
{
    public static readonly StageLayout Empty = new([], [], 0, 0);

    public StageTile[] Tiles { get; } = tiles;

    public Rect[] Cells { get; } = cells;

    public double Width { get; } = width;

    public double Height { get; } = height;
}

/// <summary>A picture drawn over the grid into a rectangle of the stage in DIPs (a call card's hole).</summary>
internal sealed record OverlayTile(StageTile Tile, Rect Rect);

/// <summary>A swap chain frame read back for the self-test.</summary>
internal sealed record StageCapture(RenderedImage Image, float ScaleX, float ScaleY, (StageTile Tile, float X, float Y, float W, float H)[] Rects);

/// <summary>A window's render thread (see the file comment).</summary>
internal sealed unsafe class StageRenderer : IDisposable
{
    private readonly CoreHost _host;
    private readonly Action<nint> _attach;
    private readonly Thread _thread;
    private readonly Lock _sizeLock = new();
    private volatile bool _stop;
    private volatile bool _simulateLoss;
    private float _dipWidth, _dipHeight, _scaleX = 1, _scaleY = 1;
    private SwapChainTarget? _target;
    private GridRenderer? _renderer;
    private GpuDevice? _targetGpu;
    private long _lastResize;
    private float _transformX, _transformY;
    private FocusAnimation? _focus;
    private StageLayout _layout = StageLayout.Empty;
    private OverlayTile[] _overlays = [];
    private TaskCompletionSource<StageCapture>? _capture;
    private string? _recover;

    private TileDraw[] _draws = new TileDraw[16];
    private ulong[] _sequences = new ulong[16];

    /// <param name="host">The core and device.</param>
    /// <param name="attach">Called (on this thread) with a new swap chain, AddRef'd for the callee to attach and release on the UI thread.</param>
    public StageRenderer(CoreHost host, Action<nint> attach)
    {
        _host = host;
        _attach = attach;
        _thread = new Thread(Run) { Name = "render", IsBackground = true };
    }

    /// <summary>Frames drawn and presented.</summary>
    public long Frames { get; private set; }

    /// <summary>Managed bytes the render thread has allocated (GC.GetAllocatedBytesForCurrentThread).</summary>
    public long AllocatedBytes => Volatile.Read(ref _allocated);

    private long _allocated;

    /// <summary>Waits that timed out (window hidden or occluded).</summary>
    public long Timeouts { get; private set; }

    /// <summary>Total buffer resizes over every swap chain.</summary>
    public long TotalResizes { get; private set; }

    /// <summary>GPU devices this thread replaced.</summary>
    public int Recoveries { get; private set; }

    /// <summary>Swap chains created (the first, and one after each device change).</summary>
    public int SwapChains { get; private set; }

    /// <summary>The swap chain's current buffer size.</summary>
    public (uint Width, uint Height) BufferSize => _target is { } t ? (t.Width, t.Height) : (0, 0);

    /// <summary>The panel size in pixels the buffers should have.</summary>
    public (uint Width, uint Height) WantedSize
    {
        get
        {
            lock (_sizeLock)
            {
                return ((uint)Math.Round(_dipWidth * _scaleX), (uint)Math.Round(_dipHeight * _scaleY));
            }
        }
    }

    /// <summary>The last error that stopped drawing, if any.</summary>
    public string? LastError { get; private set; }

    public FocusAnimation? Focus
    {
        get => Volatile.Read(ref _focus);
        set => Volatile.Write(ref _focus, value);
    }

    /// <summary>Tiles drawn through the dewarp shader by the current renderer (self-test).</summary>
    public long DewarpDraws => _renderer?.DewarpDraws ?? 0;

    /// <summary>Where fisheye cameras' circles are kept (set once by the UI thread).</summary>
    public FisheyeCircles? Circles { get; set; }

    /// <summary>The tiles and cells to draw (set by the UI thread).</summary>
    public StageLayout Layout
    {
        get => Volatile.Read(ref _layout);
        set => Volatile.Write(ref _layout, value);
    }

    /// <summary>The pictures drawn last, over the grid (set by the UI thread; replaced, never changed in place).</summary>
    public OverlayTile[] Overlays
    {
        get => Volatile.Read(ref _overlays);
        set => Volatile.Write(ref _overlays, value);
    }

    public void Start() => _thread.Start();

    /// <summary>The panel's size in DIPs and its composition scale (UI thread).</summary>
    public void SetPanelSize(double width, double height, float scaleX, float scaleY)
    {
        lock (_sizeLock)
        {
            _dipWidth = (float)width;
            _dipHeight = (float)height;
            _scaleX = scaleX > 0 ? scaleX : 1;
            _scaleY = scaleY > 0 ? scaleY : 1;
        }
    }

    /// <summary>Reads back the next frame drawn (before it is presented).</summary>
    public Task<StageCapture> CaptureAsync()
    {
        var tcs = new TaskCompletionSource<StageCapture>(TaskCreationOptions.RunContinuationsAsynchronously);
        Volatile.Write(ref _capture, tcs);
        return tcs.Task;
    }

    /// <summary>Runs the device-loss recovery on the next frame, as if Present had reported a removed device.</summary>
    public void SimulateDeviceLoss() => _simulateLoss = true;

    private void Run()
    {
        while (!_stop)
        {
            _host.FrameGate.EnterReadLock();
            try
            {
                Frame();
            }
            catch (VideoException e)
            {
                // A device lost in the middle of a call shows up here too.
                LastError = e.Message;
                AppLog.Limited("render", 5, $"render: {e.Message}");
                if (_host.Gpu.IsRemoved)
                {
                    _recover = $"device removed (0x{_host.Gpu.RemovedReason:X8})";
                }
                else
                {
                    Thread.Sleep(100);
                }
            }
            finally
            {
                _host.FrameGate.ExitReadLock();
            }
            if (_recover is { } why)
            {
                _recover = null;
                Recover(why);
            }
        }
        ReleaseTarget();
    }

    private void Frame()
    {
        (float dipW, float dipH, float sx, float sy) = ReadSize();
        uint wantW = (uint)Math.Max(1, Math.Round(dipW * sx)), wantH = (uint)Math.Max(1, Math.Round(dipH * sy));
        if (dipW < 1 || dipH < 1)
        {
            Thread.Sleep(16);
            return;
        }
        GpuDevice gpu = _host.Gpu;
        if (_target is not null && !ReferenceEquals(_targetGpu, gpu))
        {
            // Another window replaced the device: start over on the new one.
            ReleaseTarget();
        }
        if (_target is null)
        {
            _renderer = new GridRenderer(gpu, 0, 0, 0);
            _target = new SwapChainTarget(gpu, wantW, wantH);
            _targetGpu = gpu;
            _lastResize = Stopwatch.GetTimestamp();
            _transformX = _transformY = 0;
            SwapChains++;
            ((TerraFX.Interop.Windows.IUnknown*)_target.NativeSwapChain)->AddRef();
            _attach(_target.NativeSwapChain);
        }
        SwapChainTarget target = _target;
        if (!target.WaitForFrame(100))
        {
            Timeouts++;
        }
        long now = Stopwatch.GetTimestamp();
        if ((target.Width != wantW || target.Height != wantH) && now - _lastResize > Stopwatch.Frequency / 10)
        {
            int hr = target.Resize(wantW, wantH);
            if (hr < 0)
            {
                CheckLoss(hr, "ResizeBuffers");
                return;
            }
            TotalResizes++;
            _lastResize = now;
        }
        // Show the buffers at the panel's size (stretched while a resize is pending).
        float tx = dipW / target.Width, ty = dipH / target.Height;
        if (tx != _transformX || ty != _transformY)
        {
            target.SetTransform(tx, ty);
            _transformX = tx;
            _transformY = ty;
        }

        StageLayout layout = Layout;
        OverlayTile[] overlays = Overlays;
        int count = layout.Tiles.Length + overlays.Length;
        if (_draws.Length < count)
        {
            _draws = new TileDraw[count];
            _sequences = new ulong[count];
        }
        using (gpu.Lock())
        {
            int placed = Place(layout, dipW, dipH, now, target.Width / dipW, target.Height / dipH);
            PlaceOverlays(overlays, placed, target.Width / dipW, target.Height / dipH);
            _renderer!.Draw(target.View, target.Width, target.Height, _draws.AsSpan(0, count), _sequences);
        }
        for (int i = 0; i < count; i++)
        {
            ulong seq = _sequences[i];
            StageTile t = (StageTile)_draws[i].Tag!;
            if (seq != 0 && seq != t.LastSequence)
            {
                t.Shown++;
            }
            t.LastSequence = seq;
        }
        if (Interlocked.Exchange(ref _capture, null) is { } capture)
        {
            try
            {
                RenderedImage image = target.ReadBack();
                var rects = new (StageTile, float, float, float, float)[count];
                for (int i = 0; i < count; i++)
                {
                    TileDraw d = _draws[i];
                    rects[i] = ((StageTile)d.Tag!, d.DestX, d.DestY, d.DestWidth, d.DestHeight);
                }
                capture.SetResult(new StageCapture(image, target.Width / Math.Max(dipW, 1), target.Height / Math.Max(dipH, 1), rects));
            }
            catch (Exception e)
            {
                capture.SetException(e);
            }
        }
        int presented = target.Present();
        Frames++;
        Volatile.Write(ref _allocated, GC.GetAllocatedBytesForCurrentThread());
        if (presented < 0)
        {
            CheckLoss(presented, "Present");
        }
        else if (_simulateLoss)
        {
            _simulateLoss = false;
            _recover = "simulated device loss";
        }
    }

    private void CheckLoss(int hr, string what)
    {
        if (hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET || _host.Gpu.IsRemoved)
        {
            _recover = $"{what} reported 0x{hr:X8} (reason 0x{_host.Gpu.RemovedReason:X8})";
        }
        else
        {
            LastError = $"{what} failed (0x{hr:X8})";
            AppLog.Limited("present", 5, LastError);
            Thread.Sleep(50);
        }
    }

    // Outside the frame gate: replacing the device waits for every window's frame to end.
    private void Recover(string why)
    {
        AppLog.Warn($"GPU device lost: {why}; recreating it");
        GpuDevice? lost = _targetGpu;
        ReleaseTarget();
        try
        {
            _host.ReplaceGpu(lost);
            Recoveries++;
        }
        catch (VideoException e)
        {
            LastError = $"recreating the GPU device failed: {e.Message}";
            AppLog.Limited("recover", 5, LastError);
            Thread.Sleep(1000);
        }
    }

    private void ReleaseTarget()
    {
        _renderer?.Dispose();
        _renderer = null;
        _target?.Dispose();
        _target = null;
        _targetGpu = null;
    }

    private (float, float, float, float) ReadSize()
    {
        lock (_sizeLock)
        {
            return (_dipWidth, _dipHeight, _scaleX, _scaleY);
        }
    }

    // Writes the draws in buffer pixels (bx, by: buffer pixels per DIP) and
    // publishes each tile's rectangle in DIPs. The cells were laid out for
    // the layout's size; while a resize is still on its way they are scaled
    // to the panel's. Under the device lock, so picture sizes match the
    // pictures drawn. The focused tile is drawn last, over the others,
    // which stop being drawn once it covers the stage.
    private int Place(StageLayout layout, float dipW, float dipH, long now, float bx, float by)
    {
        StageTile[] tiles = layout.Tiles;
        Rect[] cells = layout.Cells;
        int count = tiles.Length;
        double kx = layout.Width > 0 ? dipW / layout.Width : 1, ky = layout.Height > 0 ? dipH / layout.Height : 1;
        FocusAnimation? focus = Focus;
        int focused = -1;
        float progress = 0;
        if (focus is not null)
        {
            focused = Array.IndexOf(tiles, focus.Tile);
            progress = FocusAnimation.Ease(focus.Progress(now));
            if (progress <= 0)
            {
                focused = -1;
            }
        }
        int n = 0;
        for (int i = 0; i <= count; i++)
        {
            // The focused tile last.
            int k = i < count ? i : focused;
            if (k < 0 || (i < count && i == focused))
            {
                continue;
            }
            StageTile t = tiles[k];
            TilePicture? p = t.Video?.ShownPicture;
            int pw = p is { HasPicture: true } ? (int)p.PictureSize.Width : 0, ph = p is { HasPicture: true } ? (int)p.PictureSize.Height : 0;
            t.PictureWidth = pw;
            t.PictureHeight = ph;
            Rect c = cells[k];
            var cell = new Rect(c.X * kx, c.Y * ky, c.Width * kx, c.Height * ky);
            // A dewarped view fills its whole cell (the views are laid out for its shape).
            FisheyeChoice? fisheye = pw > 0 && t.Fisheye is { } f && f.Mode.Dewarps() ? f : null;
            Rect dest = fisheye is null ? LayoutGeometry.Fit(pw, ph, cell) : cell;
            if (k == focused)
            {
                Rect stage = new(0, 0, dipW, dipH);
                dest = Rect.Lerp(dest, fisheye is null ? LayoutGeometry.Fit(pw, ph, stage) : stage, progress);
            }
            bool hidden = focused >= 0 && k != focused && progress >= 1;
            Draw(ref _draws[n], t, dest, pw, ph, hidden, bx, by);
            _draws[n].Dewarp = false;
            if (fisheye is not null && !hidden)
            {
                Dewarp(ref _draws[n], t, fisheye, p!, pw, ph, now);
            }
            n++;
        }
        return n;
    }

    // The call cards' pictures after the grid's draws (from index n), each
    // fitted into its card's hole; never hidden by a focused tile.
    private void PlaceOverlays(OverlayTile[] overlays, int n, float bx, float by)
    {
        foreach (OverlayTile o in overlays)
        {
            StageTile t = o.Tile;
            TilePicture? p = t.Video?.ShownPicture;
            int pw = p is { HasPicture: true } ? (int)p.PictureSize.Width : 0, ph = p is { HasPicture: true } ? (int)p.PictureSize.Height : 0;
            t.PictureWidth = pw;
            t.PictureHeight = ph;
            Draw(ref _draws[n], t, LayoutGeometry.Fit(pw, ph, o.Rect), pw, ph, false, bx, by);
            _draws[n].Dewarp = false;
            n++;
        }
    }

    // The tile's dewarp constants, recomputed only when what they depend on
    // changes; and a try at finding the camera's circle while it is unknown.
    private void Dewarp(ref TileDraw d, StageTile t, FisheyeChoice fisheye, TilePicture p, int pw, int ph, long now)
    {
        CameraRef camera = t.Camera;
        FisheyeCircles? circles = Circles;
        FisheyeCircle? found = circles?.Circle(camera);
        if (found is null && circles is not null)
        {
            double seconds = now / (double)Stopwatch.Frequency;
            if (circles.TryBegin(camera, seconds))
            {
                byte[]? luma = null;
                int w = 0, h = 0;
                try
                {
                    luma = p.ReadLuma(out w, out h);
                }
                catch (VideoException e)
                {
                    AppLog.Limited("fisheye-read", 3, $"fisheye: reading the picture failed: {e.Message}");
                }
                circles.Measure(camera, luma, w, h, seconds);
            }
        }
        FisheyeCircle circle = found ?? FisheyeCircle.Inscribed(pw, ph);
        int dw = (int)Math.Round(d.DestWidth), dh = (int)Math.Round(d.DestHeight);
        var key = (fisheye, circle, dw, dh, pw, ph);
        if (t.DewarpKey is not ValueTuple<FisheyeChoice, FisheyeCircle, int, int, int, int> old || !old.Equals(key))
        {
            var lens = new FisheyeLens(circle, pw / (double)Math.Max(ph, 1));
            t.DewarpRows = FisheyeDewarp.Of(lens, fisheye.Mount, fisheye.Mode, fisheye.Aim, Math.Max(dw, 1), Math.Max(dh, 1)).Constants();
            t.DewarpKey = key;
        }
        d.Dewarp = true;
        d.DewarpRows = t.DewarpRows;
    }

    private static void Draw(ref TileDraw d, StageTile t, Rect dest, int pw, int ph, bool hidden, float bx, float by)
    {
        d.Tile = hidden ? null : t.Video;
        d.Tag = t;
        d.DestX = (float)dest.X * bx;
        d.DestY = (float)dest.Y * by;
        d.DestWidth = (float)dest.Width * bx;
        d.DestHeight = (float)dest.Height * by;
        // Digital zoom: a smaller source around the tile's center, kept inside the picture.
        float zoom = Math.Clamp(t.Zoom, 1, 8);
        float sw = pw / zoom, sh = ph / zoom;
        float cx = t.CenterX * pw, cy = t.CenterY * ph;
        d.SrcWidth = sw;
        d.SrcHeight = sh;
        d.SrcX = zoom > 1 ? Math.Clamp(cx - sw / 2, 0, pw - sw) : 0;
        d.SrcY = zoom > 1 ? Math.Clamp(cy - sh / 2, 0, ph - sh) : 0;
        t.X = (float)dest.X;
        t.Y = (float)dest.Y;
        t.Width = (float)dest.Width;
        t.Height = (float)dest.Height;
        t.Visible = !hidden;
    }

    /// <summary>Stops the thread and releases the swap chain (detach it from the panel after).</summary>
    public void Dispose()
    {
        _stop = true;
        if (_thread.IsAlive)
        {
            _thread.Join();
        }
    }
}
