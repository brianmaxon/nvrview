/// Metal source for the grid pass, compiled once when a `GridRenderer` is
/// created (SwiftPM does not build `.metal` files outside Xcode).
///
/// One draw per tile, four vertices as a triangle strip; the quad's corners
/// come from the vertex ID and the tile's uniforms, so there is no vertex
/// buffer. `TileUniforms` must match `GridRenderer.TileUniforms`.
let gridShaderSource = """
    #include <metal_stdlib>
    using namespace metal;

    struct TileUniforms {
        float4 destination;  // clip space: left, top, right, bottom
        float4 source;       // texture coordinates: left, top, right, bottom
        float4 r;            // YCbCr-to-RGB matrix rows
        float4 g;
        float4 b;
    };

    struct Varyings {
        float4 position [[position]];
        float2 uv;
    };

    vertex Varyings tile_vertex(uint vid [[vertex_id]], constant TileUniforms &u [[buffer(0)]]) {
        float2 corner = float2(float(vid & 1), float(vid >> 1));
        Varyings out;
        out.position = float4(mix(u.destination.xy, u.destination.zw, corner), 0.0, 1.0);
        out.uv = mix(u.source.xy, u.source.zw, corner);
        return out;
    }

    fragment float4 tile_fragment(
        Varyings in [[stage_in]],
        texture2d<float> luma [[texture(0)]],
        texture2d<float> chroma [[texture(1)]],
        constant TileUniforms &u [[buffer(0)]]
    ) {
        constexpr sampler s(filter::linear, address::clamp_to_edge);
        float4 ycc = float4(luma.sample(s, in.uv).r, chroma.sample(s, in.uv).rg, 1.0);
        return float4(saturate(float3(dot(u.r, ycc), dot(u.g, ycc), dot(u.b, ycc))), 1.0);
    }
    """
