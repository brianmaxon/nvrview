import CoreGraphics

/// The 3x4 YCbCr-to-RGB matrix the pipeline computes for a tile
/// (RENDERING.md `color`), applied by the shader as
///
///     (R, G, B) = M · (Y, Cb, Cr, 1)
///
/// where Y, Cb and Cr are the 8-bit samples normalized as the GPU reads them
/// from `r8Unorm` / `rg8Unorm` textures (code / 255), and R, G, B come out
/// in 0...1 (clamped). Range offsets and scaling live in the fourth column,
/// so video and full range use the same shader.
public struct YCbCrToRGB: Sendable, Equatable {
    public var r: SIMD4<Float>
    public var g: SIMD4<Float>
    public var b: SIMD4<Float>

    public init(r: SIMD4<Float>, g: SIMD4<Float>, b: SIMD4<Float>) {
        self.r = r
        self.g = g
        self.b = b
    }

    /// From 12 floats in row-major order, as the core hands them over.
    public init?(rowMajor m: [Float]) {
        guard m.count == 12 else { return nil }
        r = SIMD4(m[0], m[1], m[2], m[3])
        g = SIMD4(m[4], m[5], m[6], m[7])
        b = SIMD4(m[8], m[9], m[10], m[11])
    }
}

/// One tile to draw: where (`destination`, in pixels of the render target,
/// origin at the top left), what part of the picture (`source`, in pixels of
/// the picture, origin at the top left; nil for the whole picture), and how
/// to convert its colors. Rectangles come from the pipeline's grid layout.
/// A tile with no picture is drawn black, like everything outside the tiles.
public struct GridTile: Sendable {
    public var picture: any TilePictureSource
    public var color: YCbCrToRGB
    public var destination: CGRect
    public var source: CGRect?
    /// A dewarped fisheye tile: drawn by the dewarp pipeline with these
    /// constants over the whole `destination` (`source` is not used).
    public var dewarp: DewarpConstants?

    public init(
        picture: any TilePictureSource, color: YCbCrToRGB, destination: CGRect, source: CGRect? = nil,
        dewarp: DewarpConstants? = nil
    ) {
        self.picture = picture
        self.color = color
        self.destination = destination
        self.source = source
        self.dewarp = dewarp
    }
}

/// A CPU copy of a rendered frame, 8-bit BGRA rows without padding.
public struct RenderedImage: Sendable, Equatable {
    public let width: Int
    public let height: Int
    public let bgra: [UInt8]

    public init(width: Int, height: Int, bgra: [UInt8]) {
        self.width = width
        self.height = height
        self.bgra = bgra
    }

    /// The pixel at (x, y), origin at the top left.
    public func rgb(x: Int, y: Int) -> SIMD3<Int> {
        let i = (y * width + x) * 4
        return SIMD3(Int(bgra[i + 2]), Int(bgra[i + 1]), Int(bgra[i]))
    }
}
