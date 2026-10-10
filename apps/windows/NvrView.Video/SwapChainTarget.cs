// A flip-model swap chain for composition (a WinUI SwapChainPanel hosts it)
// that the grid renderer draws into, paced by the swap chain's frame latency
// waitable object (docs/RENDERING.md, "The platform renderer").
//
// - Flip model (DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL), two BGRA buffers. In
//   D3D11 the runtime rotates the buffers itself: buffer 0 is always the
//   one to draw next, so one render target view of buffer 0 serves every
//   frame (D3D11 refuses writes to the other buffers of a flip-model chain).
// - Pacing: created with DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
//   and a maximum frame latency of 1. The render thread waits on the
//   waitable object before drawing each frame, so it draws once per
//   display refresh, as late as possible, and Present never blocks.
// - Resize: ResizeBuffers releases and recreates the buffers (the view is
//   released first). The owner calls it at most every so often while a
//   size is still changing and shows the old buffers stretched in the
//   meantime with SetMatrixTransform, so a live resize or an animation
//   never stalls the presentation.
//
// Every call here is made by the one render thread, under the device lock
// where the immediate context is involved.

using TerraFX.Interop.DirectX;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.DirectX.DXGI;
using static TerraFX.Interop.Windows.Windows;

namespace NvrView.Video;

/// <summary>A composition swap chain the grid is drawn into (see the file comment).</summary>
public sealed unsafe class SwapChainTarget : IDisposable
{
    private const uint BufferCount = 2;

    private readonly GpuDevice _gpu;
    private IDXGISwapChain2* _swapChain;
    private ID3D11Texture2D* _backBuffer;
    private ID3D11RenderTargetView* _view;
    private HANDLE _waitable;
    private ID3D11Texture2D* _staging;
    private uint _stagingWidth;
    private uint _stagingHeight;

    /// <summary>Creates a swap chain of <paramref name="width"/> x <paramref name="height"/> pixels.</summary>
    public SwapChainTarget(GpuDevice gpu, uint width, uint height)
    {
        _gpu = gpu;
        Width = Math.Max(width, 1);
        Height = Math.Max(height, 1);
        try
        {
            Create();
        }
        catch
        {
            Dispose();
            throw;
        }
    }

    /// <summary>Buffer width in pixels.</summary>
    public uint Width { get; private set; }

    /// <summary>Buffer height in pixels.</summary>
    public uint Height { get; private set; }

    /// <summary>The swap chain, as IDXGISwapChain1 for ISwapChainPanelNative.SetSwapChain.</summary>
    public nint NativeSwapChain => (nint)_swapChain;

    /// <summary>The view of the buffer to draw next.</summary>
    public ID3D11RenderTargetView* View => _view;

    /// <summary>Times the buffers were resized.</summary>
    public long Resizes { get; private set; }

    /// <summary>Frames presented.</summary>
    public long Presents { get; private set; }

    /// <summary>Waits for the next frame's turn; false on timeout (the window is hidden, say).</summary>
    public bool WaitForFrame(uint timeoutMs) =>
        WaitForSingleObjectEx(_waitable, timeoutMs, BOOL.TRUE) == WAIT.WAIT_OBJECT_0;

    /// <summary>Presents the drawn buffer at the next refresh. Returns the HRESULT (device loss shows here).</summary>
    public int Present()
    {
        HRESULT hr;
        using (_gpu.Lock())
        {
            hr = _swapChain->Present(1, 0);
        }
        if ((int)hr >= 0)
        {
            Presents++;
        }
        return (int)hr;
    }

    /// <summary>Resizes the buffers (the owner coalesces calls). Returns the HRESULT.</summary>
    public int Resize(uint width, uint height)
    {
        width = Math.Max(width, 1);
        height = Math.Max(height, 1);
        if (width == Width && height == Height)
        {
            return 0;
        }
        HRESULT hr;
        using (_gpu.Lock())
        {
            // Every reference to the buffers must be gone before ResizeBuffers.
            _gpu.Context->OMSetRenderTargets(0, null, null);
            ReleaseBuffers();
            _gpu.Context->Flush();
            hr = _swapChain->ResizeBuffers(BufferCount, width, height, DXGI_FORMAT.DXGI_FORMAT_B8G8R8A8_UNORM,
                (uint)DXGI_SWAP_CHAIN_FLAG.DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT);
            if ((int)hr >= 0)
            {
                Width = width;
                Height = height;
                Resizes++;
            }
            // The old buffers come back if the resize failed.
            AcquireBuffers();
        }
        return (int)hr;
    }

    /// <summary>
    /// Scales the buffers on screen: the panel shows buffer pixel (x, y) at
    /// (x * scaleX, y * scaleY) in its own units. The owner keeps it at
    /// panel size / buffer size, which also undoes the display scale.
    /// </summary>
    public void SetTransform(float scaleX, float scaleY)
    {
        DXGI_MATRIX_3X2_F m = default;
        m._11 = scaleX;
        m._22 = scaleY;
        _ = _swapChain->SetMatrixTransform(&m);
    }

    /// <summary>
    /// Reads back the buffer drawn so far (call after drawing and before
    /// Present). Waits for the GPU; for tests, allocates.
    /// </summary>
    public RenderedImage ReadBack()
    {
        ID3D11DeviceContext* ctx = _gpu.Context;
        using DeviceLock held = _gpu.Lock();
        if (_staging == null || _stagingWidth != Width || _stagingHeight != Height)
        {
            if (_staging != null)
            {
                _staging->Release();
                _staging = null;
            }
            D3D11_TEXTURE2D_DESC desc = new()
            {
                Width = Width,
                Height = Height,
                MipLevels = 1,
                ArraySize = 1,
                Format = DXGI_FORMAT.DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc = new DXGI_SAMPLE_DESC { Count = 1, Quality = 0 },
                Usage = D3D11_USAGE.D3D11_USAGE_STAGING,
                CPUAccessFlags = (uint)D3D11_CPU_ACCESS_FLAG.D3D11_CPU_ACCESS_READ,
            };
            ID3D11Texture2D* staging = null;
            GpuDevice.Check(_gpu.Device->CreateTexture2D(&desc, null, &staging), "CreateTexture2D(staging)");
            _staging = staging;
            _stagingWidth = Width;
            _stagingHeight = Height;
        }
        return GridRenderer.ReadTexture(_gpu, _backBuffer, _staging, Width, Height);
    }

    private void Create()
    {
        IDXGIDevice* dxgi = null;
        IDXGIAdapter* adapter = null;
        IDXGIFactory2* factory = null;
        IDXGISwapChain1* chain = null;
        try
        {
            GpuDevice.Check(_gpu.Device->QueryInterface(__uuidof<IDXGIDevice>(), (void**)&dxgi), "QueryInterface(IDXGIDevice)");
            GpuDevice.Check(dxgi->GetAdapter(&adapter), "GetAdapter");
            GpuDevice.Check(adapter->GetParent(__uuidof<IDXGIFactory2>(), (void**)&factory), "GetParent(IDXGIFactory2)");
            DXGI_SWAP_CHAIN_DESC1 desc = new()
            {
                Width = Width,
                Height = Height,
                Format = DXGI_FORMAT.DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo = BOOL.FALSE,
                SampleDesc = new DXGI_SAMPLE_DESC { Count = 1, Quality = 0 },
                BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount = BufferCount,
                Scaling = DXGI_SCALING.DXGI_SCALING_STRETCH,
                SwapEffect = DXGI_SWAP_EFFECT.DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode = DXGI_ALPHA_MODE.DXGI_ALPHA_MODE_IGNORE,
                Flags = (uint)DXGI_SWAP_CHAIN_FLAG.DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
            };
            using (_gpu.Lock())
            {
                GpuDevice.Check(
                    factory->CreateSwapChainForComposition((IUnknown*)_gpu.Device, &desc, null, &chain),
                    "CreateSwapChainForComposition");
            }
            IDXGISwapChain2* chain2 = null;
            GpuDevice.Check(chain->QueryInterface(__uuidof<IDXGISwapChain2>(), (void**)&chain2), "QueryInterface(IDXGISwapChain2)");
            _swapChain = chain2;
            GpuDevice.Check(_swapChain->SetMaximumFrameLatency(1), "SetMaximumFrameLatency");
            _waitable = _swapChain->GetFrameLatencyWaitableObject();
            using (_gpu.Lock())
            {
                AcquireBuffers();
            }
        }
        finally
        {
            if (chain != null)
            {
                chain->Release();
            }
            if (factory != null)
            {
                factory->Release();
            }
            if (adapter != null)
            {
                adapter->Release();
            }
            if (dxgi != null)
            {
                dxgi->Release();
            }
        }
    }

    private void AcquireBuffers()
    {
        ID3D11Texture2D* buffer = null;
        GpuDevice.Check(_swapChain->GetBuffer(0, __uuidof<ID3D11Texture2D>(), (void**)&buffer), "GetBuffer");
        _backBuffer = buffer;
        ID3D11RenderTargetView* view = null;
        GpuDevice.Check(_gpu.Device->CreateRenderTargetView((ID3D11Resource*)buffer, null, &view), "CreateRenderTargetView");
        _view = view;
    }

    private void ReleaseBuffers()
    {
        if (_view != null)
        {
            _view->Release();
            _view = null;
        }
        if (_backBuffer != null)
        {
            _backBuffer->Release();
            _backBuffer = null;
        }
    }

    /// <summary>Releases the swap chain (detach it from its panel first).</summary>
    public void Dispose()
    {
        using (_gpu.Lock())
        {
            if (_gpu.Context != null)
            {
                _gpu.Context->OMSetRenderTargets(0, null, null);
            }
            ReleaseBuffers();
            if (_staging != null)
            {
                _staging->Release();
                _staging = null;
            }
        }
        if (_waitable != HANDLE.NULL)
        {
            _ = CloseHandle(_waitable);
            _waitable = HANDLE.NULL;
        }
        if (_swapChain != null)
        {
            _swapChain->Release();
            _swapChain = null;
        }
    }
}
