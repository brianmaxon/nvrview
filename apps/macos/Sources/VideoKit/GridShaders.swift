/// Metal source for the grid pass, compiled once when a `GridRenderer` is
/// created (SwiftPM does not build `.metal` files outside Xcode).
///
/// One draw per tile, four vertices as a triangle strip; the quad's corners
/// come from the vertex ID and the tile's uniforms, so there is no vertex
/// buffer. `TileUniforms` must match `GridRenderer.TileUniforms`, and
/// `DewarpConstants` must match the Swift `DewarpConstants` (22 rows of
/// float4; docs/RENDERING.md, "Fisheye dewarping").
///
/// `dewarp_fragment` draws a fisheye tile: `in.uv` is the point of the tile
/// (0...1; the renderer sets the source to the unit square), turned into a
/// direction by the view it falls in, into a picture point by the
/// equidistant lens model, and sampled there.
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

    struct DewarpView {
        float4 rect;   // in the tile: left, top, right, bottom
        float4 axisX;  // xyz lens frame; w: 0 rectilinear, 1 cylindrical
        float4 axisY;  // w: tan(h/2) or psi0
        float4 axisZ;  // w: tan(v/2) or delta psi
        float4 extra;  // cylindrical: tau0, delta tau
    };

    struct DewarpConstants {
        float4 lens;    // cx, cy, r, r * W / H
        float4 params;  // theta max, view count
        DewarpView views[4];
    };

    fragment float4 dewarp_fragment(
        Varyings in [[stage_in]],
        texture2d<float> luma [[texture(0)]],
        texture2d<float> chroma [[texture(1)]],
        constant TileUniforms &u [[buffer(0)]],
        constant DewarpConstants &d [[buffer(1)]]
    ) {
        constexpr sampler s(filter::linear, address::clamp_to_edge);
        float2 p = in.uv;
        int count = min(int(d.params.y), 4);
        for (int i = 0; i < count; i++) {
            DewarpView v = d.views[i];
            if (p.x < v.rect.x || p.x > v.rect.z || p.y < v.rect.y || p.y > v.rect.w) continue;
            float2 t = (p - v.rect.xy) / (v.rect.zw - v.rect.xy);
            float3 dir;
            if (v.axisX.w < 0.5) {
                dir = v.axisZ.xyz + (2.0 * t.x - 1.0) * v.axisY.w * v.axisX.xyz
                    + (2.0 * t.y - 1.0) * v.axisZ.w * v.axisY.xyz;
            } else {
                float psi = v.axisY.w + t.x * v.axisZ.w;
                float tau = v.extra.x + t.y * v.extra.y;
                dir = cos(tau) * sin(psi) * v.axisX.xyz + sin(tau) * v.axisY.xyz + cos(tau) * cos(psi) * v.axisZ.xyz;
            }
            dir = normalize(dir);
            float theta = acos(clamp(dir.z, -1.0, 1.0));
            if (theta > d.params.x) return float4(0.0, 0.0, 0.0, 1.0);
            float sl = length(dir.xy);
            float2 q = sl > 1e-6 ? dir.xy / sl : float2(0.0);
            float2 uv = d.lens.xy + (theta / d.params.x) * d.lens.zw * q;
            float4 ycc = float4(luma.sample(s, uv).r, chroma.sample(s, uv).rg, 1.0);
            return float4(saturate(float3(dot(u.r, ycc), dot(u.g, ycc), dot(u.b, ycc))), 1.0);
        }
        return float4(0.0, 0.0, 0.0, 1.0);
    }
    """
