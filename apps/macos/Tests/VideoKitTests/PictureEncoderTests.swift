import Foundation
import Testing

@testable import VideoKit

/// The files Save Picture writes (docs/CAPTURE.md "Save picture"): PNG is
/// lossless, JPEG is close, neither carries metadata, and the colors keep
/// their channel order.
@Suite struct PictureEncoderTests {
    private func gradient(_ w: Int, _ h: Int) -> RenderedImage {
        var bgra = [UInt8](repeating: 255, count: w * h * 4)
        for y in 0..<h {
            for x in 0..<w {
                let i = (y * w + x) * 4
                bgra[i] = UInt8(x * 255 / (w - 1))  // blue
                bgra[i + 1] = UInt8(y * 255 / (h - 1))  // green
                bgra[i + 2] = 200  // red
            }
        }
        return RenderedImage(width: w, height: h, bgra: bgra)
    }

    @Test func pngRoundTripsExactly() throws {
        let image = gradient(40, 30)
        let data = try #require(PictureEncoder.encode(image, as: .png))
        #expect(data.prefix(4) == Data([0x89, 0x50, 0x4E, 0x47]))
        let back = try #require(PictureEncoder.decode(data))
        #expect(back.width == 40 && back.height == 30)
        #expect(back.bgra == image.bgra)
    }

    @Test func jpegIsClose() throws {
        let image = gradient(64, 48)
        let data = try #require(PictureEncoder.encode(image, as: .jpeg(quality: 0.9)))
        #expect(data.prefix(2) == Data([0xFF, 0xD8]))
        let back = try #require(PictureEncoder.decode(data))
        let p = back.rgb(x: 32, y: 24), q = image.rgb(x: 32, y: 24)
        #expect(abs(p.x - q.x) < 8 && abs(p.y - q.y) < 8 && abs(p.z - q.z) < 8)
    }

    @Test func noMetadataBeyondTheFormat() throws {
        let data = try #require(PictureEncoder.encode(gradient(16, 16), as: .png))
        let text = String(decoding: data, as: UTF8.self)
        #expect(!text.contains("Exif") && !text.contains("tEXt") && !text.contains("iTXt"))
    }
}
