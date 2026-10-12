import CoreGraphics
import CoreVideo
import Foundation
import VideoKit

/// The self-test's independent look at a saved picture: the pixels the file
/// holds against a CPU conversion of the decoded buffer it was made from
/// (the same matrix, the sampler's bilinear chroma), so the Metal path and
/// the encoder are what is being checked.
enum PictureCheck {
    /// A pixel of the buffer converted to RGB (0...255) as the shader does:
    /// luma at the pixel, chroma interpolated between its texels.
    static func rgb(
        of buffer: CVPixelBuffer, color m: YCbCrToRGB, x: Int, y: Int
    ) -> SIMD3<Int>? {
        guard CVPixelBufferGetPlaneCount(buffer) == 2 else { return nil }
        CVPixelBufferLockBaseAddress(buffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        guard let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
            let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)
        else { return nil }
        let w = CVPixelBufferGetWidthOfPlane(buffer, 0), h = CVPixelBufferGetHeightOfPlane(buffer, 0)
        guard x >= 0, y >= 0, x < w, y < h else { return nil }
        let lumaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let chromaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        let cw = CVPixelBufferGetWidthOfPlane(buffer, 1), ch = CVPixelBufferGetHeightOfPlane(buffer, 1)
        let yy = Float(luma.load(fromByteOffset: y * lumaStride + x, as: UInt8.self)) / 255
        func texel(_ cx: Int, _ cy: Int) -> (Float, Float) {
            let px = min(max(cx, 0), cw - 1), py = min(max(cy, 0), ch - 1)
            let o = py * chromaStride + px * 2
            return (
                Float(chroma.load(fromByteOffset: o, as: UInt8.self)) / 255,
                Float(chroma.load(fromByteOffset: o + 1, as: UInt8.self)) / 255
            )
        }
        let fx = (Float(x) + 0.5) * Float(cw) / Float(w) - 0.5, fy = (Float(y) + 0.5) * Float(ch) / Float(h) - 0.5
        let x0 = Int(fx.rounded(.down)), y0 = Int(fy.rounded(.down))
        let tx = fx - Float(x0), ty = fy - Float(y0)
        let a = texel(x0, y0), b = texel(x0 + 1, y0), c = texel(x0, y0 + 1), d = texel(x0 + 1, y0 + 1)
        func mix(_ p: Float, _ q: Float, _ r: Float, _ s: Float) -> Float {
            (p * (1 - tx) + q * tx) * (1 - ty) + (r * (1 - tx) + s * tx) * ty
        }
        let ycc = SIMD4<Float>(yy, mix(a.0, b.0, c.0, d.0), mix(a.1, b.1, c.1, d.1), 1)
        func channel(_ row: SIMD4<Float>) -> Int { Int((min(max((row * ycc).sum(), 0), 1) * 255).rounded()) }
        return SIMD3(channel(m.r), channel(m.g), channel(m.b))
    }

    /// How many of a grid of sample points of the saved `image` match the
    /// buffer at `region` (its top left in buffer pixels) within `tolerance`
    /// codes per channel.
    static func compare(
        _ image: RenderedImage, buffer: CVPixelBuffer, color: YCbCrToRGB, region: CGRect, tolerance: Int
    ) -> (checked: Int, matched: Int) {
        var checked = 0, matched = 0
        for j in 1...6 {
            for i in 1...6 {
                let x = image.width * i / 7, y = image.height * j / 7
                guard let want = rgb(of: buffer, color: color, x: Int(region.minX) + x, y: Int(region.minY) + y)
                else { continue }
                checked += 1
                let got = image.rgb(x: x, y: y)
                if abs(got.x - want.x) <= tolerance && abs(got.y - want.y) <= tolerance && abs(got.z - want.z) <= tolerance {
                    matched += 1
                }
            }
        }
        return (checked, matched)
    }
}
