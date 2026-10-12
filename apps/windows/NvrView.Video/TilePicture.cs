// The one decoded picture a tile keeps for drawing (docs/RENDERING.md,
// "The platform renderer"): newest wins, older ones are overwritten in
// place. Two shapes, chosen by where the decoder's output lives:
//
// - hardware decoding: one NV12 texture the decoder's output surface is
//   copied into on the GPU, read through an R8 view (luma) and an R8G8
//   view (chroma);
// - software decoding: an R8 and an R8G8 texture filled from the decoder's
//   NV12 output in memory.
//
// Either way the renderer sees two views and a color matrix. Every member
// is used only while holding the device lock (GpuDevice.Lock).

using NvrView.Interop;
using TerraFX.Interop.DirectX;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;

namespace NvrView.Video;

/// <summary>The core's YCbCr-to-RGB matrix: row-major 3x4, samples as code/255.</summary>
public unsafe struct ColorMatrix
{
    /// <summary>The 12 coefficients.</summary>
    public fixed float M[12];

    /// <summary>Copies the matrix of a Configure action.</summary>
    public static ColorMatrix From(in NvColor color)
    {
        ColorMatrix m = default;
        for (int i = 0; i < 12; i++)
        {
            m.M[i] = color.YcbcrToRgb[i];
        }
        return m;
    }

    /// <summary>
    /// The matrix for luma coefficients <paramref name="kr"/> and <paramref name="kb"/> over 8-bit samples read as code / 255,
    /// video or full range (the same as the macOS app's YCbCrToRGB.standard), for pictures decoded outside the core.
    /// </summary>
    public static ColorMatrix Standard(float kr, float kb, bool fullRange)
    {
        float kg = 1 - kr - kb;
        float ys = fullRange ? 1 : 255f / 219f, yo = fullRange ? 0 : -16f / 219f;
        float cs = fullRange ? 1 : 255f / 224f, co = fullRange ? -128f / 255f : -128f / 224f;
        float crR = 2 * (1 - kr), cbB = 2 * (1 - kb);
        float cbG = -2 * kb * (1 - kb) / kg, crG = -2 * kr * (1 - kr) / kg;
        ColorMatrix m = default;
        float[] rows =
        [
            ys, 0, crR * cs, yo + crR * co,
            ys, cbG * cs, crG * cs, yo + (cbG + crG) * co,
            ys, cbB * cs, 0, yo + cbB * co,
        ];
        for (int i = 0; i < 12; i++)
        {
            m.M[i] = rows[i];
        }
        return m;
    }

    /// <summary>BT.601 (<paramref name="bt709"/> false) or BT.709 coefficients.</summary>
    public static ColorMatrix Of(bool bt709, bool fullRange) =>
        bt709 ? Standard(0.2126f, 0.0722f, fullRange) : Standard(0.299f, 0.114f, fullRange);

    /// <summary>
    /// BT.601 video range, for pictures decoded outside the core (benchmarks);
    /// the core always supplies its own.
    /// </summary>
    public static ColorMatrix Bt601VideoRange()
    {
        // (R, G, B) = M * (Y, Cb, Cr, 1) with samples as code/255.
        const float ys = 255f / 219f, cs = 255f / 224f;
        ColorMatrix m = default;
        float[] rows =
        [
            ys, 0, 1.402f * cs, 0,
            ys, -0.344136f * cs, -0.714136f * cs, 0,
            ys, 1.772f * cs, 0, 0,
        ];
        for (int r = 0; r < 3; r++)
        {
            float offset = -16f / 255f * ys;
            offset -= 128f / 255f * (rows[r * 4 + 1] + rows[r * 4 + 2]);
            rows[r * 4 + 3] = offset;
        }
        for (int i = 0; i < 12; i++)
        {
            m.M[i] = rows[i];
        }
        return m;
    }
}

/// <summary>A tile's held picture (see the file comment). Use only under the device lock.</summary>
public sealed unsafe class TilePicture : IDisposable
{
    private readonly GpuDevice _gpu;
    private ID3D11Texture2D* _nv12;
    private ID3D11Texture2D* _luma;
    private ID3D11Texture2D* _chroma;
    private ID3D11ShaderResourceView* _lumaView;
    private ID3D11ShaderResourceView* _chromaView;

    /// <summary>Creates an empty picture on <paramref name="gpu"/>.</summary>
    public TilePicture(GpuDevice gpu) => _gpu = gpu;

    /// <summary>Width of the textures (the decoder's output, possibly the coded size).</summary>
    public uint TextureWidth { get; private set; }

    /// <summary>Height of the textures.</summary>
    public uint TextureHeight { get; private set; }

    /// <summary>The visible picture inside the textures (the Configure's visible_rect).</summary>
    public NvRect Visible { get; set; }

    /// <summary>The matrix the shader applies.</summary>
    public ColorMatrix Color;

    /// <summary>True while there is a picture to draw (false before the first and after Clear).</summary>
    public bool HasPicture { get; private set; }

    /// <summary>Timestamp of the held picture, µs.</summary>
    public long PtsUs { get; private set; }

    /// <summary>Grows with every new picture or Clear.</summary>
    public ulong Sequence { get; private set; }

    /// <summary>The visible size, for the grid layout; zero without a picture.</summary>
    public NvSize PictureSize =>
        HasPicture ? new NvSize(Visible.Width, Visible.Height) : default;

    internal ID3D11ShaderResourceView* LumaView => _lumaView;

    internal ID3D11ShaderResourceView* ChromaView => _chromaView;

    /// <summary>Copies a decoder output surface (NV12, on this device) in on the GPU.</summary>
    internal void CopyFrom(ID3D11Texture2D* source, uint subresource, uint width, uint height, long ptsUs)
    {
        if (_nv12 == null || TextureWidth != width || TextureHeight != height)
        {
            Release();
            CreateNv12(width, height);
        }
        D3D11_BOX box = new() { left = 0, top = 0, front = 0, right = width, bottom = height, back = 1 };
        _gpu.Context->CopySubresourceRegion(
            (ID3D11Resource*)_nv12, 0, 0, 0, 0, (ID3D11Resource*)source, subresource, &box);
        Published(ptsUs);
    }

    /// <summary>
    /// Uploads an NV12 picture from memory: <paramref name="height"/> luma
    /// rows of <paramref name="pitch"/> bytes, then the interleaved chroma
    /// plane at <paramref name="chromaOffset"/>.
    /// </summary>
    internal void Upload(byte* data, int pitch, int chromaOffset, uint width, uint height, long ptsUs)
    {
        if (_luma == null || TextureWidth != width || TextureHeight != height)
        {
            Release();
            CreatePlanes(width, height);
        }
        _gpu.Context->UpdateSubresource((ID3D11Resource*)_luma, 0, null, data, (uint)pitch, 0);
        _gpu.Context->UpdateSubresource((ID3D11Resource*)_chroma, 0, null, data + chromaOffset, (uint)pitch, 0);
        Published(ptsUs);
    }

    /// <summary>
    /// Copies the visible luma plane to memory (8-bit, rows of <paramref name="width"/> bytes),
    /// or null without a picture. Waits for the GPU and allocates a staging texture and the
    /// array each time: for the few tries of finding a fisheye camera's image circle
    /// (docs/RENDERING.md), never per frame. Call under the device lock.
    /// </summary>
    public byte[]? ReadLuma(out int width, out int height)
    {
        width = 0;
        height = 0;
        ID3D11Texture2D* source = _nv12 != null ? _nv12 : _luma;
        if (!HasPicture || source == null)
        {
            return null;
        }
        D3D11_TEXTURE2D_DESC desc;
        source->GetDesc(&desc);
        desc.Usage = D3D11_USAGE.D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = (uint)D3D11_CPU_ACCESS_FLAG.D3D11_CPU_ACCESS_READ;
        desc.MiscFlags = 0;
        ID3D11Texture2D* staging = null;
        GpuDevice.Check(_gpu.Device->CreateTexture2D(&desc, null, &staging), "CreateTexture2D(luma readback)");
        try
        {
            ID3D11DeviceContext* ctx = _gpu.Context;
            ctx->CopyResource((ID3D11Resource*)staging, (ID3D11Resource*)source);
            D3D11_MAPPED_SUBRESOURCE mapped;
            GpuDevice.Check(ctx->Map((ID3D11Resource*)staging, 0, D3D11_MAP.D3D11_MAP_READ, 0, &mapped), "Map(luma readback)");
            try
            {
                NvRect v = Visible;
                int x0 = (int)Math.Min(v.X, desc.Width), y0 = (int)Math.Min(v.Y, desc.Height);
                width = (int)Math.Min(v.Width, desc.Width - (uint)x0);
                height = (int)Math.Min(v.Height, desc.Height - (uint)y0);
                if (width <= 0 || height <= 0)
                {
                    width = (int)desc.Width;
                    height = (int)desc.Height;
                    x0 = 0;
                    y0 = 0;
                }
                byte[] pixels = new byte[width * height];
                for (int y = 0; y < height; y++)
                {
                    new ReadOnlySpan<byte>((byte*)mapped.pData + (y0 + y) * mapped.RowPitch + x0, width)
                        .CopyTo(pixels.AsSpan(y * width, width));
                }
                return pixels;
            }
            finally
            {
                ctx->Unmap((ID3D11Resource*)staging, 0);
            }
        }
        finally
        {
            staging->Release();
        }
    }

    /// <summary>
    /// Copies the visible picture to memory as its planes: <paramref name="luma"/> (rows of <paramref name="width"/>
    /// bytes) and <paramref name="chroma"/> (interleaved Cb, Cr at half the size, rounded up, starting at the visible
    /// origin halved). For checking what the renderer draws against a CPU conversion (the self-test); waits for the
    /// GPU and allocates. Call under the device lock. False without a picture.
    /// </summary>
    public bool ReadYuv(out int width, out int height, out byte[] luma, out byte[] chroma)
    {
        width = 0;
        height = 0;
        luma = [];
        chroma = [];
        if (!HasPicture || (_nv12 == null && _luma == null))
        {
            return false;
        }
        NvRect v = Visible;
        int x0 = (int)Math.Min(v.X, TextureWidth), y0 = (int)Math.Min(v.Y, TextureHeight);
        width = (int)Math.Min(v.Width, TextureWidth - (uint)x0);
        height = (int)Math.Min(v.Height, TextureHeight - (uint)y0);
        if (width <= 0 || height <= 0)
        {
            width = 0;
            height = 0;
            return false;
        }
        int cw = (width + 1) / 2, ch = (height + 1) / 2, cx0 = x0 / 2, cy0 = y0 / 2;
        luma = new byte[width * height];
        chroma = new byte[cw * ch * 2];
        ID3D11DeviceContext* ctx = _gpu.Context;
        if (_nv12 != null)
        {
            ID3D11Texture2D* staging = Staging(_nv12);
            try
            {
                ctx->CopyResource((ID3D11Resource*)staging, (ID3D11Resource*)_nv12);
                D3D11_MAPPED_SUBRESOURCE mapped;
                GpuDevice.Check(ctx->Map((ID3D11Resource*)staging, 0, D3D11_MAP.D3D11_MAP_READ, 0, &mapped), "Map(yuv readback)");
                try
                {
                    // The chroma plane follows the luma plane's rows in an NV12 surface.
                    byte* chromaBase = (byte*)mapped.pData + (long)mapped.RowPitch * TextureHeight;
                    CopyPlane((byte*)mapped.pData, mapped.RowPitch, x0, y0, width, height, 1, luma);
                    CopyPlane(chromaBase, mapped.RowPitch, cx0, cy0, cw, ch, 2, chroma);
                }
                finally
                {
                    ctx->Unmap((ID3D11Resource*)staging, 0);
                }
            }
            finally
            {
                staging->Release();
            }
            return true;
        }
        ID3D11Texture2D* lumaStaging = Staging(_luma);
        ID3D11Texture2D* chromaStaging = Staging(_chroma);
        try
        {
            ctx->CopyResource((ID3D11Resource*)lumaStaging, (ID3D11Resource*)_luma);
            ctx->CopyResource((ID3D11Resource*)chromaStaging, (ID3D11Resource*)_chroma);
            D3D11_MAPPED_SUBRESOURCE mapped;
            GpuDevice.Check(ctx->Map((ID3D11Resource*)lumaStaging, 0, D3D11_MAP.D3D11_MAP_READ, 0, &mapped), "Map(luma readback)");
            CopyPlane((byte*)mapped.pData, mapped.RowPitch, x0, y0, width, height, 1, luma);
            ctx->Unmap((ID3D11Resource*)lumaStaging, 0);
            GpuDevice.Check(ctx->Map((ID3D11Resource*)chromaStaging, 0, D3D11_MAP.D3D11_MAP_READ, 0, &mapped), "Map(chroma readback)");
            CopyPlane((byte*)mapped.pData, mapped.RowPitch, cx0, cy0, cw, ch, 2, chroma);
            ctx->Unmap((ID3D11Resource*)chromaStaging, 0);
        }
        finally
        {
            lumaStaging->Release();
            chromaStaging->Release();
        }
        return true;
    }

    /// <summary>The color matrix's 12 coefficients, row-major 3x4 (for a CPU conversion in the self-test).</summary>
    public float[] ColorRows()
    {
        var rows = new float[12];
        for (int i = 0; i < 12; i++)
        {
            rows[i] = Color.M[i];
        }
        return rows;
    }

    private ID3D11Texture2D* Staging(ID3D11Texture2D* source)
    {
        D3D11_TEXTURE2D_DESC desc;
        source->GetDesc(&desc);
        desc.Usage = D3D11_USAGE.D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = (uint)D3D11_CPU_ACCESS_FLAG.D3D11_CPU_ACCESS_READ;
        desc.MiscFlags = 0;
        ID3D11Texture2D* staging = null;
        GpuDevice.Check(_gpu.Device->CreateTexture2D(&desc, null, &staging), "CreateTexture2D(readback)");
        return staging;
    }

    // Copies a w x h window (origin x0, y0) of a plane with bytesPerTexel bytes per texel into dest, rows packed.
    private static void CopyPlane(byte* plane, uint pitch, int x0, int y0, int w, int h, int bytesPerTexel, byte[] dest)
    {
        int row = w * bytesPerTexel;
        for (int y = 0; y < h; y++)
        {
            new ReadOnlySpan<byte>(plane + (long)(y0 + y) * pitch + x0 * bytesPerTexel, row).CopyTo(dest.AsSpan(y * row, row));
        }
    }

    /// <summary>Drops the picture (the tile draws black); keeps the textures for reuse.</summary>
    public void Clear()
    {
        HasPicture = false;
        Sequence++;
    }

    private void Published(long ptsUs)
    {
        HasPicture = true;
        PtsUs = ptsUs;
        Sequence++;
    }

    private void CreateNv12(uint width, uint height)
    {
        D3D11_TEXTURE2D_DESC desc = new()
        {
            Width = width,
            Height = height,
            MipLevels = 1,
            ArraySize = 1,
            Format = DXGI_FORMAT.DXGI_FORMAT_NV12,
            SampleDesc = new DXGI_SAMPLE_DESC { Count = 1, Quality = 0 },
            Usage = D3D11_USAGE.D3D11_USAGE_DEFAULT,
            BindFlags = (uint)D3D11_BIND_FLAG.D3D11_BIND_SHADER_RESOURCE,
        };
        ID3D11Texture2D* tex = null;
        GpuDevice.Check(_gpu.Device->CreateTexture2D(&desc, null, &tex), "CreateTexture2D(NV12)");
        _nv12 = tex;
        // An R8 view reads the luma plane of NV12, an R8G8 view the chroma plane.
        _lumaView = View((ID3D11Resource*)tex, DXGI_FORMAT.DXGI_FORMAT_R8_UNORM);
        _chromaView = View((ID3D11Resource*)tex, DXGI_FORMAT.DXGI_FORMAT_R8G8_UNORM);
        TextureWidth = width;
        TextureHeight = height;
    }

    private void CreatePlanes(uint width, uint height)
    {
        _luma = Plane(width, height, DXGI_FORMAT.DXGI_FORMAT_R8_UNORM);
        _chroma = Plane((width + 1) / 2, (height + 1) / 2, DXGI_FORMAT.DXGI_FORMAT_R8G8_UNORM);
        _lumaView = View((ID3D11Resource*)_luma, DXGI_FORMAT.DXGI_FORMAT_R8_UNORM);
        _chromaView = View((ID3D11Resource*)_chroma, DXGI_FORMAT.DXGI_FORMAT_R8G8_UNORM);
        TextureWidth = width;
        TextureHeight = height;
    }

    private ID3D11Texture2D* Plane(uint width, uint height, DXGI_FORMAT format)
    {
        D3D11_TEXTURE2D_DESC desc = new()
        {
            Width = width,
            Height = height,
            MipLevels = 1,
            ArraySize = 1,
            Format = format,
            SampleDesc = new DXGI_SAMPLE_DESC { Count = 1, Quality = 0 },
            Usage = D3D11_USAGE.D3D11_USAGE_DEFAULT,
            BindFlags = (uint)D3D11_BIND_FLAG.D3D11_BIND_SHADER_RESOURCE,
        };
        ID3D11Texture2D* tex = null;
        GpuDevice.Check(_gpu.Device->CreateTexture2D(&desc, null, &tex), "CreateTexture2D(plane)");
        return tex;
    }

    private ID3D11ShaderResourceView* View(ID3D11Resource* resource, DXGI_FORMAT format)
    {
        D3D11_SHADER_RESOURCE_VIEW_DESC desc = default;
        desc.Format = format;
        desc.ViewDimension = D3D_SRV_DIMENSION.D3D11_SRV_DIMENSION_TEXTURE2D;
        desc.Texture2D.MostDetailedMip = 0;
        desc.Texture2D.MipLevels = 1;
        ID3D11ShaderResourceView* view = null;
        GpuDevice.Check(_gpu.Device->CreateShaderResourceView(resource, &desc, &view), "CreateShaderResourceView");
        return view;
    }

    private void Release()
    {
        ReleaseOne(ref _lumaView);
        ReleaseOne(ref _chromaView);
        ReleaseOne(ref _nv12);
        ReleaseOne(ref _luma);
        ReleaseOne(ref _chroma);
        TextureWidth = 0;
        TextureHeight = 0;
    }

    private static void ReleaseOne<T>(ref T* p)
        where T : unmanaged, IUnknown.Interface
    {
        if (p != null)
        {
            p->Release();
            p = null;
        }
    }

    /// <summary>Releases the textures. Call under the device lock.</summary>
    public void Dispose()
    {
        HasPicture = false;
        Release();
    }
}
