// Draws tiles into an offscreen BGRA render target in one pass, with the
// core's grid layout (nv_grid_layout) and each picture's color matrix
// (docs/RENDERING.md). One draw per tile: four vertices as a triangle strip
// whose corners come from the vertex ID and the tile's constants, NV12 as
// two views (R8 luma, R8G8 chroma). Render() lays tiles out as a grid and
// draws offscreen (the headless demo reads the result back to check real
// pixels); Draw() takes rectangles the caller computed (the app's animated
// focus and digital zoom) and any render target, such as a swap chain's
// buffer (SwapChainTarget).
//
// A tile that carries fisheye dewarp constants (docs/RENDERING.md,
// "Fisheye dewarping") is drawn with a second pixel shader that maps each
// output pixel to a direction and samples the circular picture there; its
// constants (22 rows, NvrView.Tiles.DewarpConstants) are a second constant
// buffer, and a third holds where the visible picture sits in the textures.

using System.Runtime.InteropServices;
using NvrView.Interop;
using NvrView.Tiles;
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

/// <summary>
/// One tile to draw: its destination in render-target pixels and the part
/// of its visible picture to sample, in visible-picture pixels (a smaller
/// source than the picture is a crop or a digital zoom).
/// </summary>
public struct TileDraw
{
    /// <summary>The tile whose shown picture is drawn; null draws nothing.</summary>
    public VideoTile? Tile;

    /// <summary>A picture to draw instead of the tile's (a recorded file's, for the follow cut and grids); used under the device lock.</summary>
    public TilePicture? Picture;

    /// <summary>Destination, render-target pixels.</summary>
    public float DestX, DestY, DestWidth, DestHeight;

    /// <summary>Source, visible-picture pixels.</summary>
    public float SrcX, SrcY, SrcWidth, SrcHeight;

    /// <summary>The caller's own reference (which of its tiles this is); not used for drawing.</summary>
    public object? Tag;

    /// <summary>Draw through the fisheye dewarp shader with <see cref="DewarpRows"/> (the source rectangle is then unused: the views cover the destination).</summary>
    public bool Dewarp;

    /// <summary>The dewarp constants when <see cref="Dewarp"/> is set.</summary>
    public DewarpConstants DewarpRows;
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

        // Fisheye dewarping (docs/RENDERING.md): i.uv is the point in the tile
        // (0..1). Rows per view: rect, axis X (w: 0 rectilinear, 1 cylindrical),
        // axis Y (w: tan(h/2) or psi0), axis Z (w: tan(v/2) or dpsi), extra
        // (tau0, dtau).
        cbuffer Dewarp : register(b1)
        {
            float4 lens;      // cx, cy, r, r*W/H (picture fractions)
            float4 params;    // thetaMax, view count
            float4 views[20];
        };
        cbuffer Picture : register(b2)
        {
            float4 visible;   // the visible picture in texture coordinates: origin xy, size zw
        };

        float4 psDewarp(Varyings i) : SV_Target
        {
            float2 p = i.uv;
            int count = (int)params.y;
            [unroll] for (int k = 0; k < 4; k++)
            {
                if (k >= count)
                {
                    break;
                }
                float4 rect = views[k * 5];
                float4 ax = views[k * 5 + 1];
                float4 ay = views[k * 5 + 2];
                float4 az = views[k * 5 + 3];
                float4 ex = views[k * 5 + 4];
                if (p.x < rect.x || p.x > rect.z || p.y < rect.y || p.y > rect.w)
                {
                    continue;
                }
                float2 t = (p - rect.xy) / (rect.zw - rect.xy);
                float3 dir;
                if (ax.w < 0.5)
                {
                    dir = az.xyz + (2 * t.x - 1) * ay.w * ax.xyz + (2 * t.y - 1) * az.w * ay.xyz;
                }
                else
                {
                    float psi = ay.w + t.x * az.w;
                    float tau = ex.x + t.y * ex.y;
                    dir = cos(tau) * sin(psi) * ax.xyz + sin(tau) * ay.xyz + cos(tau) * cos(psi) * az.xyz;
                }
                dir = normalize(dir);
                float theta = acos(clamp(dir.z, -1, 1));
                if (theta > params.x)
                {
                    return float4(0, 0, 0, 1);
                }
                float sl = length(dir.xy);
                float2 q = sl > 1e-6 ? dir.xy / sl : float2(0, 0);
                float2 uv = visible.xy + (lens.xy + (theta / params.x) * lens.zw * q) * visible.zw;
                float4 ycc = float4(luma.SampleLevel(linearClamp, uv, 0), chroma.SampleLevel(linearClamp, uv, 0), 1);
                return float4(saturate(float3(dot(r, ycc), dot(g, ycc), dot(b, ycc))), 1);
            }
            return float4(0, 0, 0, 1);
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
    private ID3D11PixelShader* _psDewarp;
    private ID3D11Buffer* _constants;
    private ID3D11Buffer* _dewarp;
    private ID3D11Buffer* _picture;
    private ID3D11SamplerState* _sampler;
    private ID3D11RasterizerState* _rasterizer;
    private NvSize[] _sizes = new NvSize[16];
    private NvTileLayout[] _layout = new NvTileLayout[16];
    private TileDraw[] _draws = new TileDraw[16];
    private int _count;

    /// <summary>
    /// A renderer with an offscreen target of <paramref name="width"/> x
    /// <paramref name="height"/> pixels for <see cref="Render"/>; 0 x 0 for
    /// one that only draws into targets given to <see cref="Draw"/>.
    /// </summary>
    /// <param name="gpu">The shared device.</param>
    /// <param name="width">Offscreen target width (0: none).</param>
    /// <param name="height">Offscreen target height (0: none).</param>
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

    /// <summary>Tiles drawn through the dewarp shader, over every frame (the self-test checks it grows).</summary>
    public long DewarpDraws { get; private set; }

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
        if (_targetView == null)
        {
            throw new InvalidOperationException("this renderer has no offscreen target");
        }
        if (_sizes.Length < tiles.Length)
        {
            _sizes = new NvSize[tiles.Length];
            _layout = new NvTileLayout[tiles.Length];
            _draws = new TileDraw[tiles.Length];
        }
        _count = tiles.Length;
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
        for (int i = 0; i < tiles.Length; i++)
        {
            NvTileLayout l = _layout[i];
            _draws[i] = new TileDraw
            {
                Tile = _sizes[i].Width == 0 ? null : tiles[i],
                DestX = l.Dest.X,
                DestY = l.Dest.Y,
                DestWidth = l.Dest.Width,
                DestHeight = l.Dest.Height,
                SrcX = l.Source.X,
                SrcY = l.Source.Y,
                SrcWidth = l.Source.Width,
                SrcHeight = l.Source.Height,
            };
        }
        Draw(_targetView, Width, Height, _draws.AsSpan(0, _count), sequences);
    }

    /// <summary>
    /// Draws one frame into <paramref name="target"/> (<paramref name="width"/>
    /// x <paramref name="height"/> pixels): clears it to black, then draws each
    /// tile's shown picture in order (later ones on top). <paramref name="sequences"/>
    /// (one per draw) receives the sequence number of each picture drawn, 0 for
    /// none. Takes the device lock; allocates nothing.
    /// </summary>
    public void Draw(ID3D11RenderTargetView* target, uint width, uint height, ReadOnlySpan<TileDraw> draws, Span<ulong> sequences)
    {
        if (sequences.Length < draws.Length)
        {
            throw new ArgumentException("one sequence per draw", nameof(sequences));
        }
        ID3D11DeviceContext* ctx = _gpu.Context;
        using DeviceLock held = _gpu.Lock();
        ctx->OMSetRenderTargets(1, &target, null);
        D3D11_VIEWPORT viewport = new() { TopLeftX = 0, TopLeftY = 0, Width = width, Height = height, MinDepth = 0, MaxDepth = 1 };
        ctx->RSSetViewports(1, &viewport);
        // Element by element: a stackalloc initializer of floats allocates
        // an array on the heap each time (measured).
        float* black = stackalloc float[4];
        black[0] = 0;
        black[1] = 0;
        black[2] = 0;
        black[3] = 1;
        ctx->ClearRenderTargetView(target, black);
        ctx->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY.D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        ctx->IASetInputLayout(null);
        ctx->VSSetShader(_vs, null, 0);
        ctx->PSSetShader(_ps, null, 0);
        ctx->RSSetState(_rasterizer);
        ID3D11SamplerState* sampler = _sampler;
        ctx->PSSetSamplers(0, 1, &sampler);
        ID3D11Buffer* cb = _constants;
        ctx->VSSetConstantBuffers(0, 1, &cb);
        ID3D11Buffer** psBuffers = stackalloc ID3D11Buffer*[3];
        psBuffers[0] = _constants;
        psBuffers[1] = _dewarp;
        psBuffers[2] = _picture;
        ctx->PSSetConstantBuffers(0, 3, psBuffers);
        bool dewarping = false;

        ID3D11ShaderResourceView** views = stackalloc ID3D11ShaderResourceView*[2];
        for (int i = 0; i < draws.Length; i++)
        {
            ref readonly TileDraw d = ref draws[i];
            TilePicture? p = d.Picture ?? d.Tile?.ShownPicture;
            if (p is null || !p.HasPicture || p.LumaView == null)
            {
                sequences[i] = 0;
                continue;
            }
            sequences[i] = p.Sequence;
            if (d.DestWidth <= 0 || d.DestHeight <= 0 || (!d.Dewarp && (d.SrcWidth <= 0 || d.SrcHeight <= 0)))
            {
                continue;
            }
            if (d.Dewarp != dewarping)
            {
                dewarping = d.Dewarp;
                ctx->PSSetShader(dewarping ? _psDewarp : _ps, null, 0);
            }
            D3D11_MAPPED_SUBRESOURCE mapped;
            GpuDevice.Check(ctx->Map((ID3D11Resource*)cb, 0, D3D11_MAP.D3D11_MAP_WRITE_DISCARD, 0, &mapped), "Map(constants)");
            var c = (TileConstants*)mapped.pData;
            c->DestLeft = d.DestX / width * 2 - 1;
            c->DestRight = (d.DestX + d.DestWidth) / width * 2 - 1;
            c->DestTop = 1 - d.DestY / height * 2;
            c->DestBottom = 1 - (d.DestY + d.DestHeight) / height * 2;
            // Source rectangles are in visible-picture pixels; the textures
            // may hold the coded picture, with the visible one inside.
            float tw = p.TextureWidth, th = p.TextureHeight;
            NvRect v = p.Visible;
            if (d.Dewarp)
            {
                // The shader works in the tile's own coordinates.
                c->SrcLeft = 0;
                c->SrcTop = 0;
                c->SrcRight = 1;
                c->SrcBottom = 1;
            }
            else
            {
                c->SrcLeft = (v.X + d.SrcX) / tw;
                c->SrcRight = (v.X + d.SrcX + d.SrcWidth) / tw;
                c->SrcTop = (v.Y + d.SrcY) / th;
                c->SrcBottom = (v.Y + d.SrcY + d.SrcHeight) / th;
            }
            for (int k = 0; k < 12; k++)
            {
                c->Matrix[k] = p.Color.M[k];
            }
            ctx->Unmap((ID3D11Resource*)cb, 0);
            if (d.Dewarp)
            {
                GpuDevice.Check(ctx->Map((ID3D11Resource*)_dewarp, 0, D3D11_MAP.D3D11_MAP_WRITE_DISCARD, 0, &mapped), "Map(dewarp)");
                *(DewarpConstants*)mapped.pData = d.DewarpRows;
                ctx->Unmap((ID3D11Resource*)_dewarp, 0);
                GpuDevice.Check(ctx->Map((ID3D11Resource*)_picture, 0, D3D11_MAP.D3D11_MAP_WRITE_DISCARD, 0, &mapped), "Map(picture)");
                var vis = (float*)mapped.pData;
                vis[0] = v.X / tw;
                vis[1] = v.Y / th;
                vis[2] = v.Width / tw;
                vis[3] = v.Height / th;
                ctx->Unmap((ID3D11Resource*)_picture, 0);
                DewarpDraws++;
            }
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

    /// <summary>
    /// Draws the given rectangles into the offscreen target (an edit's frame, an export's): the pictures are the draws' own
    /// (<see cref="TileDraw.Picture"/>). Then <see cref="ReadBack"/> reads the frame.
    /// </summary>
    public void RenderDraws(ReadOnlySpan<TileDraw> draws, Span<ulong> sequences)
    {
        if (_targetView == null)
        {
            throw new InvalidOperationException("this renderer has no offscreen target");
        }
        Draw(_targetView, Width, Height, draws, sequences);
    }

    /// <summary>Reads back the last frame (waits for the GPU). For tests; allocates.</summary>
    public RenderedImage ReadBack()
    {
        if (_target == null)
        {
            throw new InvalidOperationException("this renderer has no offscreen target");
        }
        return ReadTexture(_gpu, _target, _staging, Width, Height);
    }

    /// <summary>
    /// Copies a BGRA texture into <paramref name="staging"/> (same size, CPU
    /// readable) and reads it out. Waits for the GPU; allocates.
    /// </summary>
    public static RenderedImage ReadTexture(GpuDevice gpu, ID3D11Texture2D* source, ID3D11Texture2D* staging, uint width, uint height)
    {
        ID3D11DeviceContext* ctx = gpu.Context;
        byte[] pixels = new byte[width * height * 4];
        using DeviceLock held = gpu.Lock();
        ctx->CopyResource((ID3D11Resource*)staging, (ID3D11Resource*)source);
        D3D11_MAPPED_SUBRESOURCE mapped;
        GpuDevice.Check(ctx->Map((ID3D11Resource*)staging, 0, D3D11_MAP.D3D11_MAP_READ, 0, &mapped), "Map(readback)");
        try
        {
            int row = (int)width * 4;
            for (int y = 0; y < height; y++)
            {
                new ReadOnlySpan<byte>((byte*)mapped.pData + y * mapped.RowPitch, row)
                    .CopyTo(pixels.AsSpan(y * row, row));
            }
        }
        finally
        {
            ctx->Unmap((ID3D11Resource*)staging, 0);
        }
        return new RenderedImage((int)width, (int)height, pixels);
    }

    private void Create()
    {
        ID3D11Device* device = _gpu.Device;
        if (Width > 0 && Height > 0)
        {
            CreateOffscreen(device);
        }
        CreatePipeline(device);
    }

    private void CreateOffscreen(ID3D11Device* device)
    {
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
    }

    private void CreatePipeline(ID3D11Device* device)
    {
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
        using (ShaderBlob ps = Compile("psDewarp"u8, "ps_4_0"u8))
        {
            ID3D11PixelShader* shader = null;
            GpuDevice.Check(device->CreatePixelShader(ps.Pointer, ps.Size, null, &shader), "CreatePixelShader(dewarp)");
            _psDewarp = shader;
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
        cbDesc.ByteWidth = (uint)sizeof(DewarpConstants);
        cb = null;
        GpuDevice.Check(device->CreateBuffer(&cbDesc, null, &cb), "CreateBuffer(dewarp)");
        _dewarp = cb;
        cbDesc.ByteWidth = 16;
        cb = null;
        GpuDevice.Check(device->CreateBuffer(&cbDesc, null, &cb), "CreateBuffer(picture)");
        _picture = cb;

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
        Release(ref _picture);
        Release(ref _dewarp);
        Release(ref _constants);
        Release(ref _psDewarp);
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
