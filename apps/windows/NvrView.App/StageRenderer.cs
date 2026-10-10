// The render thread: draws the tile grid into the window's swap chain once
// per display refresh (docs/UX.md "Full screen and focus", docs/RENDERING.md
// "The platform renderer").
//
// - Pacing: waits on the swap chain's frame latency waitable, then lays the
//   tiles out, draws, and presents. It keeps doing so during resizes and
//   focus animations; nothing is allocated per frame in steady state.
// - Resize: the panel's size (from the UI thread) is followed by
//   ResizeBuffers at most every 100 ms while it keeps changing, then once
//   more at the exact size; in between the old buffers are shown stretched
//   (SetMatrixTransform), so presentation never stops.
// - Layout: nv_grid_layout in panel pixels, the focused tile's rectangle
//   animated between its grid cell and the whole stage, digital zoom as a
//   smaller source rectangle. Each tile's rectangle (in DIPs) is published
//   on its StageTile for the UI thread's chrome and hit testing.
// - Device loss: a Present that reports the device removed or reset (or a
//   simulated loss in the self-test) releases the swap chain and renderer,
//   has CoreHost replace the device and move every tile to it, and starts
//   again with a new swap chain, which the UI thread attaches to the panel.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Video;
using static TerraFX.Interop.DirectX.DXGI;

namespace NvrView.App;

/// <summary>The focused tile and its animation (immutable; replaced on each change).</summary>
internal sealed class FocusAnimation(ulong tile, long start, bool entering, float from)
{
    public const double Seconds = 0.25;

    public ulong Tile { get; } = tile;

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

/// <summary>A swap chain frame read back for the self-test.</summary>
internal sealed record StageCapture(RenderedImage Image, float ScaleX, float ScaleY, (ulong Tile, float X, float Y, float W, float H)[] Rects);

/// <summary>The render thread (see the file comment).</summary>
internal sealed unsafe class StageRenderer : IDisposable
{
    private const uint Gap = 4;

    private readonly CoreHost _host;
    private readonly Viewer _viewer;
    private readonly Action<nint> _attach;
    private readonly Thread _thread;
    private readonly Lock _sizeLock = new();
    private volatile bool _stop;
    private volatile bool _simulateLoss;
    private float _dipWidth, _dipHeight, _scaleX = 1, _scaleY = 1;
    private SwapChainTarget? _target;
    private GridRenderer? _renderer;
    private long _lastResize;
    private float _transformX, _transformY;
    private FocusAnimation? _focus;
    private TaskCompletionSource<StageCapture>? _capture;

    private NvSize[] _sizes = new NvSize[Viewer.MaxTiles];
    private NvTileLayout[] _layout = new NvTileLayout[Viewer.MaxTiles];
    private TileDraw[] _draws = new TileDraw[Viewer.MaxTiles];
    private ulong[] _sequences = new ulong[Viewer.MaxTiles];
    private readonly NvSize[] _one = new NvSize[1];
    private readonly NvTileLayout[] _oneLayout = new NvTileLayout[1];

    /// <param name="host">The core and device.</param>
    /// <param name="viewer">The tiles.</param>
    /// <param name="attach">Called (on this thread) with a new swap chain, AddRef'd for the callee to attach and release on the UI thread.</param>
    public StageRenderer(CoreHost host, Viewer viewer, Action<nint> attach)
    {
        _host = host;
        _viewer = viewer;
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

    /// <summary>Times the swap chain buffers were resized.</summary>
    public long Resizes => _target?.Resizes ?? 0;

    /// <summary>Total buffer resizes over every swap chain.</summary>
    public long TotalResizes { get; private set; }

    /// <summary>GPU devices replaced.</summary>
    public int Recoveries { get; private set; }

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
                    Recover($"device removed (0x{_host.Gpu.RemovedReason:X8})");
                }
                else
                {
                    Thread.Sleep(100);
                }
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
        if (_target is null)
        {
            _renderer = new GridRenderer(_host.Gpu, 0, 0, Gap);
            _target = new SwapChainTarget(_host.Gpu, wantW, wantH);
            _lastResize = Stopwatch.GetTimestamp();
            _transformX = _transformY = 0;
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

        StageTile[] tiles = _viewer.Tiles;
        int count = tiles.Length;
        if (_draws.Length < count)
        {
            _sizes = new NvSize[count];
            _layout = new NvTileLayout[count];
            _draws = new TileDraw[count];
            _sequences = new ulong[count];
        }
        GpuDevice gpu = _host.Gpu;
        using (gpu.Lock())
        {
            Layout(tiles, wantW, wantH, now, target.Width / (float)wantW, target.Height / (float)wantH, sx, sy);
            _renderer!.Draw(target.View, target.Width, target.Height, _draws.AsSpan(0, count), _sequences);
        }
        for (int i = 0; i < count; i++)
        {
            ulong seq = _sequences[i];
            StageTile t = DrawnTile(i, tiles);
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
                var rects = new (ulong, float, float, float, float)[count];
                for (int i = 0; i < count; i++)
                {
                    TileDraw d = _draws[i];
                    rects[i] = (DrawnTile(i, tiles).Id, d.DestX, d.DestY, d.DestWidth, d.DestHeight);
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
            Recover("simulated device loss");
        }
    }

    // Draw order puts the focused tile last; this maps a draw back to its tile.
    private StageTile DrawnTile(int drawIndex, StageTile[] tiles) =>
        (StageTile?)_draws[drawIndex].Tag ?? tiles[drawIndex];

    private void CheckLoss(int hr, string what)
    {
        if (hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET || _host.Gpu.IsRemoved)
        {
            Recover($"{what} reported 0x{hr:X8} (reason 0x{_host.Gpu.RemovedReason:X8})");
        }
        else
        {
            LastError = $"{what} failed (0x{hr:X8})";
            AppLog.Limited("present", 5, LastError);
            Thread.Sleep(50);
        }
    }

    private void Recover(string why)
    {
        AppLog.Warn($"GPU device lost: {why}; recreating it");
        ReleaseTarget();
        try
        {
            _host.ReplaceGpu();
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
    }

    private (float, float, float, float) ReadSize()
    {
        lock (_sizeLock)
        {
            return (_dipWidth, _dipHeight, _scaleX, _scaleY);
        }
    }

    // Lays the tiles out in panel pixels (w x h), writes the draws in buffer
    // pixels (bx, by: buffer per panel pixel), and publishes each tile's
    // rectangle in DIPs (sx, sy: panel pixels per DIP). Under the device lock,
    // so the picture sizes match the pictures drawn.
    private void Layout(StageTile[] tiles, uint w, uint h, long now, float bx, float by, float sx, float sy)
    {
        int count = tiles.Length;
        for (int i = 0; i < count; i++)
        {
            TilePicture? p = tiles[i].Video.ShownPicture;
            _sizes[i] = p is { HasPicture: true } ? p.PictureSize : default;
            tiles[i].PictureWidth = (int)_sizes[i].Width;
            tiles[i].PictureHeight = (int)_sizes[i].Height;
        }
        if (count > 0)
        {
            NvCore.GridLayout(new NvSize(w, h), Gap, _sizes.AsSpan(0, count), false, _layout.AsSpan(0, count));
        }
        FocusAnimation? focus = Focus;
        int focused = -1;
        float progress = 0;
        if (focus is not null)
        {
            for (int i = 0; i < count; i++)
            {
                if (tiles[i].Id == focus.Tile)
                {
                    focused = i;
                    break;
                }
            }
            progress = FocusAnimation.Ease(focus.Progress(now));
            if (progress <= 0)
            {
                focused = -1;
            }
        }
        // Grid tiles first, the focused tile last (on top); fully focused,
        // only the focused tile is drawn.
        int n = 0;
        for (int i = 0; i < count; i++)
        {
            StageTile t = tiles[i];
            bool hidden = focused >= 0 && i != focused && progress >= 1;
            if (i == focused)
            {
                continue;
            }
            NvTileLayout l = _layout[i];
            Place(ref _draws[n], t, l.Dest.X, l.Dest.Y, l.Dest.Width, l.Dest.Height, l.Source, hidden, bx, by, sx, sy);
            n++;
        }
        if (focused >= 0)
        {
            StageTile t = tiles[focused];
            NvTileLayout cell = _layout[focused];
            _one[0] = _sizes[focused];
            NvCore.GridLayout(new NvSize(w, h), 0, _one, false, _oneLayout);
            NvTileLayout full = _oneLayout[0];
            float x = Lerp(cell.Dest.X, full.Dest.X, progress), y = Lerp(cell.Dest.Y, full.Dest.Y, progress);
            float dw = Lerp(cell.Dest.Width, full.Dest.Width, progress), dh = Lerp(cell.Dest.Height, full.Dest.Height, progress);
            Place(ref _draws[n], t, x, y, dw, dh, full.Source, false, bx, by, sx, sy);
            n++;
        }
    }

    private static void Place(ref TileDraw d, StageTile t, float x, float y, float w, float h, NvRect source,
        bool hidden, float bx, float by, float sx, float sy)
    {
        d.Tile = hidden ? null : t.Video;
        d.Tag = t;
        d.DestX = x * bx;
        d.DestY = y * by;
        d.DestWidth = w * bx;
        d.DestHeight = h * by;
        // Digital zoom: a smaller source around the tile's center, kept inside the picture.
        float zoom = Math.Clamp(t.Zoom, 1, 8);
        float sw = source.Width / zoom, sh = source.Height / zoom;
        float pw = t.PictureWidth, ph = t.PictureHeight;
        float cx = t.CenterX * pw, cy = t.CenterY * ph;
        d.SrcWidth = sw;
        d.SrcHeight = sh;
        d.SrcX = zoom > 1 ? Math.Clamp(cx - sw / 2, source.X, source.X + source.Width - sw) : source.X;
        d.SrcY = zoom > 1 ? Math.Clamp(cy - sh / 2, source.Y, source.Y + source.Height - sh) : source.Y;
        t.X = x / sx;
        t.Y = y / sy;
        t.Width = w / sx;
        t.Height = h / sy;
        t.Visible = !hidden;
    }

    private static float Lerp(float a, float b, float t) => a + (b - a) * t;

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
