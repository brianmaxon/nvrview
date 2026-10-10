// Draws tiles into an offscreen BGRA render target in one pass, with the
// core's grid layout (nv_grid_layout) and each picture's color matrix
// (docs/RENDERING.md). One draw per tile: four vertices as a triangle strip
// whose corners come from the vertex ID and the tile's constants, NV12 as
// two views (R8 luma, R8G8 chroma). The same pass will draw into a swap
// chain's back buffer in the app; here it draws offscreen and can read the
// result back, which is how the self-test checks real pixels.

using System.Runtime.InteropServices;
using NvrView.Interop;
using TerraFX.Interop.DirectX;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.DirectX.DirectX;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.DirectX.D3DCOMPILE;

namespace NvrView.Video;

/// <summary>A rendered frame read back from the GPU: BGRA, top row first, no padding.</summary>
public sealed class RenderedImage(int width, int height, byte[] bgra)
{
    /// <summary>Width in pixels.</summary>
    public int Width { get; } = width;

    /// <summary>Height in pixels.</summary>
    public int Height { get; } = height;

    /// <summary>The pixels, 4 bytes each (B, G, R, A).</summary>
    public byte[] Bgra { get; } = bgra;

    /// <summary>The (R, G, B) of one pixel.</summary>
    public (int R, int G, int B) Rgb(int x, int y)
    {
        int i = (y * Width + x) * 4;
        return (Bgra[i + 2], Bgra[i + 1], Bgra[i]);
    }
}

/// <summary>The grid pass (see the file comment).</summary>
public sealed unsafe class GridRenderer : IDisposable
{
    private const string Shader = """
        cbuffer Tile : register(b0)
        {
            float4 destination; // clip space: left, top, right, bottom
            float4 source;      // texture coordinates: left, top, right, bottom
            float4 r;           // YCbCr-to-RGB matrix rows
            float4 g;
            float4 b;
        };
        Texture2D<float> luma : register(t0);
        Texture2D<float2> chroma : register(t1);
        SamplerState linearClamp : register(s0);

        struct Varyings
        {
            float4 position : SV_Position;
            float2 uv : TEXCOORD0;
        };

        Varyings vs(uint id : SV_VertexID)
        {
            float2 corner = float2(id & 1, id >> 1);
            Varyings o;
            o.position = float4(lerp(destination.xy, destination.zw, corner), 0, 1);
            o.uv = lerp(source.xy, source.zw, corner);
            return o;
        }

        float4 ps(Varyings i) : SV_Target
        {
            float4 ycc = float4(luma.Sample(linearClamp, i.uv), chroma.Sample(linearClamp, i.uv), 1);
            return float4(saturate(float3(dot(r, ycc), dot(g, ycc), dot(b, ycc))), 1);
        }
        """;

    [StructLayout(LayoutKind.Sequential)]
    private struct TileConstants
    {
        public float DestLeft, DestTop, DestRight, DestBottom;
        public float SrcLeft, SrcTop, SrcRight, SrcBottom;
        public fixed float Matrix[12];
    }

    private readonly GpuDevice _gpu;
    private ID3D11Texture2D* _target;
    private ID3D11RenderTargetView* _targetView;
    private ID3D11Texture2D* _staging;
    private ID3D11VertexShader* _vs;
    private ID3D11PixelShader* _ps;
    private ID3D11Buffer* _constants;
    private ID3D11SamplerState* _sampler;
    private ID3D11RasterizerState* _rasterizer;
    private NvSize[] _sizes = new NvSize[16];
    private NvTileLayout[] _layout = new NvTileLayout[16];
    private int _count;

    /// <summary>A renderer drawing <paramref name="width"/> x <paramref name="height"/> pixels.</summary>
    /// <param name="gpu">The shared device.</param>
    /// <param name="width">Render target width.</param>
    /// <param name="height">Render target height.</param>
    /// <param name="gap">Pixels between grid cells.</param>
    /// <param name="fill">Crop to fill each cell instead of letterboxing.</param>
    public GridRenderer(GpuDevice gpu, uint width, uint height, uint gap = 4, bool fill = false)
    {
        _gpu = gpu;
        Width = width;
        Height = height;
        Gap = gap;
        Fill = fill;
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

    /// <summary>Render target width.</summary>
    public uint Width { get; }

    /// <summary>Render target height.</summary>
    public uint Height { get; }

    /// <summary>Pixels between grid cells.</summary>
    public uint Gap { get; }

    /// <summary>Crop to fill instead of letterboxing.</summary>
    public bool Fill { get; }

    /// <summary>Frames rendered.</summary>
    public long Frames { get; private set; }

    /// <summary>The layout used by the last frame, one entry per tile.</summary>
    public ReadOnlySpan<NvTileLayout> Layout => _layout.AsSpan(0, _count);

    /// <summary>The picture sizes the last frame was laid out with (zero for tiles without a picture).</summary>
    public ReadOnlySpan<NvSize> PictureSizes => _sizes.AsSpan(0, _count);

    /// <summary>
    /// Draws one frame: every tile's shown picture in its grid cell, black
    /// where there is none. <paramref name="sequences"/> (one per tile)
    /// receives the sequence number of each picture drawn, 0 for none.
    /// </summary>
    public void Render(ReadOnlySpan<VideoTile?> tiles, Span<ulong> sequences)
    {
        if (sequences.Length < tiles.Length)
        {
            throw new ArgumentException("one sequence per tile", nameof(sequences));
        }
        if (_sizes.Length < tiles.Length)
        {
            _sizes = new NvSize[tiles.Length];
            _layout = new NvTileLayout[tiles.Length];
        }
        _count = tiles.Length;
        ID3D11DeviceContext* ctx = _gpu.Context;
        using DeviceLock held = _gpu.Lock();
        for (int i = 0; i < tiles.Length; i++)
        {
            TilePicture? p = tiles[i]?.ShownPicture;
            _sizes[i] = p is { HasPicture: true } ? p.PictureSize : default;
        }
        if (_count > 0)
        {
            NvCore.GridLayout(new NvSize(Width, Height), Gap, _sizes.AsSpan(0, _count), Fill, _layout.AsSpan(0, _count));
        }

        ID3D11RenderTargetView* rtv = _targetView;
        ctx->OMSetRenderTargets(1, &rtv, null);
        D3D11_VIEWPORT viewport = new() { TopLeftX = 0, TopLeftY = 0, Width = Width, Height = Height, MinDepth = 0, MaxDepth = 1 };
        ctx->RSSetViewports(1, &viewport);
        float* black = stackalloc float[4] { 0, 0, 0, 1 };
        ctx->ClearRenderTargetView(rtv, black);
        ctx->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY.D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        ctx->IASetInputLayout(null);
        ctx->VSSetShader(_vs, null, 0);
        ctx->PSSetShader(_ps, null, 0);
        ctx->RSSetState(_rasterizer);
        ID3D11SamplerState* sampler = _sampler;
        ctx->PSSetSamplers(0, 1, &sampler);
        ID3D11Buffer* cb = _constants;
        ctx->VSSetConstantBuffers(0, 1, &cb);
        ctx->PSSetConstantBuffers(0, 1, &cb);

        ID3D11ShaderResourceView** views = stackalloc ID3D11ShaderResourceView*[2];
        for (int i = 0; i < tiles.Length; i++)
        {
            TilePicture? p = tiles[i]?.ShownPicture;
            if (p is null || !p.HasPicture || p.LumaView == null || _sizes[i].Width == 0)
            {
                sequences[i] = 0;
                continue;
            }
            sequences[i] = p.Sequence;
            NvTileLayout l = _layout[i];
            if (l.Dest.Width == 0 || l.Dest.Height == 0)
            {
                continue;
            }
            D3D11_MAPPED_SUBRESOURCE mapped;
            GpuDevice.Check(ctx->Map((ID3D11Resource*)cb, 0, D3D11_MAP.D3D11_MAP_WRITE_DISCARD, 0, &mapped), "Map(constants)");
            var c = (TileConstants*)mapped.pData;
            c->DestLeft = l.Dest.X / (float)Width * 2 - 1;
            c->DestRight = (l.Dest.X + l.Dest.Width) / (float)Width * 2 - 1;
            c->DestTop = 1 - l.Dest.Y / (float)Height * 2;
            c->DestBottom = 1 - (l.Dest.Y + l.Dest.Height) / (float)Height * 2;
            // Source rectangles are in visible-picture pixels; the textures
            // may hold the coded picture, with the visible one inside.
            float tw = p.TextureWidth, th = p.TextureHeight;
            NvRect v = p.Visible;
            c->SrcLeft = (v.X + l.Source.X) / tw;
            c->SrcRight = (v.X + l.Source.X + l.Source.Width) / tw;
            c->SrcTop = (v.Y + l.Source.Y) / th;
            c->SrcBottom = (v.Y + l.Source.Y + l.Source.Height) / th;
            for (int k = 0; k < 12; k++)
            {
                c->Matrix[k] = p.Color.M[k];
            }
            ctx->Unmap((ID3D11Resource*)cb, 0);
            views[0] = p.LumaView;
            views[1] = p.ChromaView;
            ctx->PSSetShaderResources(0, 2, views);
            ctx->Draw(4, 0);
        }
        // Unbind the pictures so decoders can write them without a hazard.
        views[0] = null;
        views[1] = null;
        ctx->PSSetShaderResources(0, 2, views);
        ctx->Flush();
        Frames++;
    }

    /// <summary>Reads back the last frame (waits for the GPU). For tests; allocates.</summary>
    public RenderedImage ReadBack()
    {
        ID3D11DeviceContext* ctx = _gpu.Context;
        byte[] pixels = new byte[Width * Height * 4];
        using DeviceLock held = _gpu.Lock();
        ctx->CopyResource((ID3D11Resource*)_staging, (ID3D11Resource*)_target);
        D3D11_MAPPED_SUBRESOURCE mapped;
        GpuDevice.Check(ctx->Map((ID3D11Resource*)_staging, 0, D3D11_MAP.D3D11_MAP_READ, 0, &mapped), "Map(readback)");
        try
        {
            int row = (int)Width * 4;
            for (int y = 0; y < Height; y++)
            {
                new ReadOnlySpan<byte>((byte*)mapped.pData + y * mapped.RowPitch, row)
                    .CopyTo(pixels.AsSpan(y * row, row));
            }
        }
        finally
        {
            ctx->Unmap((ID3D11Resource*)_staging, 0);
        }
        return new RenderedImage((int)Width, (int)Height, pixels);
    }

    private void Create()
    {
        ID3D11Device* device = _gpu.Device;
        D3D11_TEXTURE2D_DESC desc = new()
        {
            Width = Width,
            Height = Height,
            MipLevels = 1,
            ArraySize = 1,
            Format = DXGI_FORMAT.DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc = new DXGI_SAMPLE_DESC { Count = 1, Quality = 0 },
            Usage = D3D11_USAGE.D3D11_USAGE_DEFAULT,
            BindFlags = (uint)D3D11_BIND_FLAG.D3D11_BIND_RENDER_TARGET,
        };
        ID3D11Texture2D* target = null;
        GpuDevice.Check(device->CreateTexture2D(&desc, null, &target), "CreateTexture2D(target)");
        _target = target;
        ID3D11RenderTargetView* rtv = null;
        GpuDevice.Check(device->CreateRenderTargetView((ID3D11Resource*)target, null, &rtv), "CreateRenderTargetView");
        _targetView = rtv;

        desc.Usage = D3D11_USAGE.D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = (uint)D3D11_CPU_ACCESS_FLAG.D3D11_CPU_ACCESS_READ;
        ID3D11Texture2D* staging = null;
        GpuDevice.Check(device->CreateTexture2D(&desc, null, &staging), "CreateTexture2D(staging)");
        _staging = staging;

        using (ShaderBlob vs = Compile("vs"u8, "vs_4_0"u8))
        {
            ID3D11VertexShader* shader = null;
            GpuDevice.Check(device->CreateVertexShader(vs.Pointer, vs.Size, null, &shader), "CreateVertexShader");
            _vs = shader;
        }
        using (ShaderBlob ps = Compile("ps"u8, "ps_4_0"u8))
        {
            ID3D11PixelShader* shader = null;
            GpuDevice.Check(device->CreatePixelShader(ps.Pointer, ps.Size, null, &shader), "CreatePixelShader");
            _ps = shader;
        }

        D3D11_BUFFER_DESC cbDesc = new()
        {
            ByteWidth = (uint)sizeof(TileConstants),
            Usage = D3D11_USAGE.D3D11_USAGE_DYNAMIC,
            BindFlags = (uint)D3D11_BIND_FLAG.D3D11_BIND_CONSTANT_BUFFER,
            CPUAccessFlags = (uint)D3D11_CPU_ACCESS_FLAG.D3D11_CPU_ACCESS_WRITE,
        };
        ID3D11Buffer* cb = null;
        GpuDevice.Check(device->CreateBuffer(&cbDesc, null, &cb), "CreateBuffer(constants)");
        _constants = cb;

        D3D11_SAMPLER_DESC sd = new()
        {
            Filter = D3D11_FILTER.D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU = D3D11_TEXTURE_ADDRESS_MODE.D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV = D3D11_TEXTURE_ADDRESS_MODE.D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW = D3D11_TEXTURE_ADDRESS_MODE.D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc = D3D11_COMPARISON_FUNC.D3D11_COMPARISON_NEVER,
            MaxLOD = float.MaxValue,
        };
        ID3D11SamplerState* sampler = null;
        GpuDevice.Check(device->CreateSamplerState(&sd, &sampler), "CreateSamplerState");
        _sampler = sampler;

        D3D11_RASTERIZER_DESC rd = new()
        {
            FillMode = D3D11_FILL_MODE.D3D11_FILL_SOLID,
            CullMode = D3D11_CULL_MODE.D3D11_CULL_NONE,
            DepthClipEnable = BOOL.TRUE,
        };
        ID3D11RasterizerState* rs = null;
        GpuDevice.Check(device->CreateRasterizerState(&rd, &rs), "CreateRasterizerState");
        _rasterizer = rs;
    }

    private static ShaderBlob Compile(ReadOnlySpan<byte> entry, ReadOnlySpan<byte> target)
    {
        byte[] source = System.Text.Encoding.UTF8.GetBytes(Shader);
        // NUL-terminated copies of the entry point and target names.
        Span<byte> e = stackalloc byte[entry.Length + 1];
        Span<byte> t = stackalloc byte[target.Length + 1];
        entry.CopyTo(e);
        target.CopyTo(t);
        e[^1] = 0;
        t[^1] = 0;
        ID3DBlob* code = null;
        ID3DBlob* errors = null;
        HRESULT hr;
        fixed (byte* src = source)
        fixed (byte* ep = e)
        fixed (byte* tp = t)
        {
            hr = D3DCompile(src, (nuint)source.Length, null, null, null, (sbyte*)ep, (sbyte*)tp,
                D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &code, &errors);
        }
        string? message = null;
        if (errors != null)
        {
            message = Marshal.PtrToStringAnsi((nint)errors->GetBufferPointer(), (int)errors->GetBufferSize());
            errors->Release();
        }
        if ((int)hr < 0 || code == null)
        {
            throw new InvalidOperationException($"shader compile failed (0x{(int)hr:X8}): {message}");
        }
        return new ShaderBlob(code);
    }

    private readonly struct ShaderBlob(ID3DBlob* blob) : IDisposable
    {
        public void* Pointer => blob->GetBufferPointer();

        public nuint Size => blob->GetBufferSize();

        public void Dispose() => blob->Release();
    }

    /// <summary>Releases the GPU objects.</summary>
    public void Dispose()
    {
        Release(ref _rasterizer);
        Release(ref _sampler);
        Release(ref _constants);
        Release(ref _ps);
        Release(ref _vs);
        Release(ref _staging);
        Release(ref _targetView);
        Release(ref _target);
    }

    private static void Release<T>(ref T* p)
        where T : unmanaged, IUnknown.Interface
    {
        if (p != null)
        {
            p->Release();
            p = null;
        }
    }
}
