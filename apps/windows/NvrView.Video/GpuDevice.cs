// The one D3D11 device that decoders and the renderer share: hardware with
// video support when there is a GPU, WARP (the software rasterizer) when
// there is none or when asked. Media Foundation decoders reach it through a
// DXGI device manager; everything else locks it with ID3D11Multithread.

using System.Runtime.Versioning;
using TerraFX.Interop.DirectX;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.DirectX.DirectX;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.DirectX.D3D11;
using static TerraFX.Interop.Windows.E;
using static TerraFX.Interop.Windows.MF;
using static TerraFX.Interop.Windows.S;

[assembly: SupportedOSPlatform("windows10.0.17763.0")]

namespace NvrView.Video;

/// <summary>Which D3D11 device to create.</summary>
public enum GpuChoice
{
    /// <summary>The default hardware adapter with video support; WARP if that fails.</summary>
    Auto,

    /// <summary>The default hardware adapter with video support, or fail.</summary>
    Hardware,

    /// <summary>WARP, the software rasterizer (no video decoding on the GPU).</summary>
    Warp,
}

/// <summary>A D3D11 or Media Foundation call failed.</summary>
public sealed class VideoException(string what, int hresult)
    : Exception($"{what} failed (0x{hresult:X8})")
{
    /// <summary>The HRESULT.</summary>
    public int HResult32 { get; } = hresult;
}

/// <summary>
/// The shared D3D11 device. Multithread protected: any thread may use it
/// while holding <see cref="Lock"/>, and Media Foundation decoders use it
/// through <see cref="Manager"/>.
/// </summary>
public sealed unsafe class GpuDevice : IDisposable
{
    private static int s_mfStarted;

    private ID3D11Device* _device;
    private ID3D11DeviceContext* _context;
    private ID3D11Multithread* _multithread;
    private IMFDXGIDeviceManager* _manager;

    private GpuDevice()
    {
    }

    /// <summary>The device. Valid until Dispose.</summary>
    public ID3D11Device* Device => _device;

    /// <summary>The immediate context; use it only while holding <see cref="Lock"/>.</summary>
    public ID3D11DeviceContext* Context => _context;

    /// <summary>The device manager for D3D11-aware decoders; null without video support.</summary>
    public IMFDXGIDeviceManager* Manager => _manager;

    /// <summary>The adapter's name as DXGI reports it.</summary>
    public string AdapterName { get; private set; } = "";

    /// <summary>True for WARP or the Basic Render Driver (no GPU).</summary>
    public bool IsSoftware { get; private set; }

    /// <summary>True when the device was created with video (DXVA) support.</summary>
    public bool HasVideo => _manager != null;

    /// <summary>The feature level the device runs at.</summary>
    public D3D_FEATURE_LEVEL FeatureLevel { get; private set; }

    /// <summary>One line for reports: adapter, kind, video support.</summary>
    public string Describe() =>
        $"{AdapterName} ({(IsSoftware ? "software" : "hardware")}, " +
        $"feature level {((int)FeatureLevel >> 12) & 0xF}.{((int)FeatureLevel >> 8) & 0xF}, " +
        $"video {(HasVideo ? "yes" : "no")})";

    /// <summary>Starts Media Foundation once per process (never shut down).</summary>
    public static void StartMediaFoundation()
    {
        if (Interlocked.CompareExchange(ref s_mfStarted, 1, 0) == 0)
        {
            Com.EnsureMta();
            Check(MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET), "MFStartup");
        }
    }

    /// <summary>Creates the device.</summary>
    public static GpuDevice Create(GpuChoice choice)
    {
        StartMediaFoundation();
        var gpu = new GpuDevice();
        try
        {
            bool created = false;
            if (choice != GpuChoice.Warp)
            {
                created = gpu.TryCreate(D3D_DRIVER_TYPE.D3D_DRIVER_TYPE_HARDWARE, video: true);
                if (!created && choice == GpuChoice.Auto)
                {
                    // A GPU without a video engine, or the Basic Render Driver.
                    created = gpu.TryCreate(D3D_DRIVER_TYPE.D3D_DRIVER_TYPE_HARDWARE, video: false);
                }
            }
            if (!created && choice != GpuChoice.Hardware)
            {
                created = gpu.TryCreate(D3D_DRIVER_TYPE.D3D_DRIVER_TYPE_WARP, video: false);
            }
            if (!created)
            {
                throw new VideoException("D3D11CreateDevice", E_FAIL);
            }
            gpu.Finish();
            return gpu;
        }
        catch
        {
            gpu.Dispose();
            throw;
        }
    }

    private bool TryCreate(D3D_DRIVER_TYPE type, bool video)
    {
        uint flags = (uint)D3D11_CREATE_DEVICE_FLAG.D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        if (video)
        {
            flags |= (uint)D3D11_CREATE_DEVICE_FLAG.D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        }
        D3D_FEATURE_LEVEL* levels = stackalloc D3D_FEATURE_LEVEL[]
        {
            D3D_FEATURE_LEVEL.D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL.D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL.D3D_FEATURE_LEVEL_10_1,
            D3D_FEATURE_LEVEL.D3D_FEATURE_LEVEL_10_0,
        };
        ID3D11Device* device = null;
        ID3D11DeviceContext* context = null;
        D3D_FEATURE_LEVEL level;
        HRESULT hr = D3D11CreateDevice(
            null, type, HMODULE.NULL, flags, levels, 4, D3D11_SDK_VERSION, &device, &level, &context);
        if ((int)hr < 0)
        {
            return false;
        }
        _device = device;
        _context = context;
        FeatureLevel = level;
        if (video)
        {
            IMFDXGIDeviceManager* manager = null;
            uint token = 0;
            hr = MFCreateDXGIDeviceManager(&token, &manager);
            if ((int)hr >= 0)
            {
                hr = manager->ResetDevice((IUnknown*)device, token);
                if ((int)hr < 0)
                {
                    manager->Release();
                    manager = null;
                }
            }
            _manager = manager;
        }
        return true;
    }

    private void Finish()
    {
        ID3D11Multithread* mt = null;
        Check(_device->QueryInterface(__uuidof<ID3D11Multithread>(), (void**)&mt), "QueryInterface(ID3D11Multithread)");
        mt->SetMultithreadProtected(BOOL.TRUE);
        _multithread = mt;

        IDXGIDevice* dxgi = null;
        IDXGIAdapter* adapter = null;
        try
        {
            Check(_device->QueryInterface(__uuidof<IDXGIDevice>(), (void**)&dxgi), "QueryInterface(IDXGIDevice)");
            Check(dxgi->GetAdapter(&adapter), "IDXGIDevice.GetAdapter");
            DXGI_ADAPTER_DESC desc;
            Check(adapter->GetDesc(&desc), "IDXGIAdapter.GetDesc");
            AdapterName = new string((char*)&desc.Description).TrimEnd('\0');
            // WARP and the Basic Render Driver: Microsoft's vendor ID 0x1414.
            IsSoftware = desc.VendorId == 0x1414;
        }
        finally
        {
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

    /// <summary>
    /// Holds the device's lock (ID3D11Multithread) until disposed. Every use
    /// of <see cref="Context"/> and every change to a <see cref="TilePicture"/>
    /// happens inside it.
    /// </summary>
    public DeviceLock Lock() => new(_multithread);

    /// <summary>Waits until the GPU has finished everything submitted so far.</summary>
    public void WaitForIdle()
    {
        D3D11_QUERY_DESC desc = new() { Query = D3D11_QUERY.D3D11_QUERY_EVENT };
        ID3D11Query* query = null;
        Check(_device->CreateQuery(&desc, &query), "CreateQuery");
        try
        {
            using (Lock())
            {
                _context->End((ID3D11Asynchronous*)query);
                _context->Flush();
            }
            BOOL done = BOOL.FALSE;
            while (true)
            {
                HRESULT hr;
                using (Lock())
                {
                    hr = _context->GetData((ID3D11Asynchronous*)query, &done, (uint)sizeof(BOOL), 0);
                }
                if ((int)hr < 0 || ((int)hr == S_OK && done != BOOL.FALSE))
                {
                    return;
                }
                Thread.Yield();
            }
        }
        finally
        {
            query->Release();
        }
    }

    /// <summary>Releases the device (after every decoder and renderer using it).</summary>
    public void Dispose()
    {
        if (_manager != null)
        {
            _manager->Release();
            _manager = null;
        }
        if (_multithread != null)
        {
            _multithread->Release();
            _multithread = null;
        }
        if (_context != null)
        {
            _context->ClearState();
            _context->Flush();
            _context->Release();
            _context = null;
        }
        if (_device != null)
        {
            _device->Release();
            _device = null;
        }
    }

    internal static void Check(HRESULT hr, string what)
    {
        if ((int)hr < 0)
        {
            throw new VideoException(what, (int)hr);
        }
    }
}

/// <summary>The device lock, held from <see cref="GpuDevice.Lock"/> until Dispose.</summary>
public readonly unsafe ref struct DeviceLock
{
    private readonly ID3D11Multithread* _mt;

    internal DeviceLock(ID3D11Multithread* mt)
    {
        _mt = mt;
        mt->Enter();
    }

    /// <summary>Leaves the lock.</summary>
    public void Dispose() => _mt->Leave();
}

/// <summary>COM on the calling thread: Media Foundation needs it for decoder creation.</summary>
internal static unsafe class Com
{
    [ThreadStatic] private static bool t_ready;

    /// <summary>
    /// Joins the multithreaded apartment once per thread (core threads are
    /// not COM threads). Never uninitialized: the thread may still hold COM
    /// objects when it ends, and an ending thread releases COM by itself.
    /// </summary>
    public static void EnsureMta()
    {
        if (!t_ready)
        {
            // S_FALSE (already in the MTA) and RPC_E_CHANGED_MODE (an STA
            // thread: COM works there too) are both fine.
            _ = CoInitializeEx(null, (uint)COINIT.COINIT_MULTITHREADED);
            t_ready = true;
        }
    }
}
