import CoreGraphics
import CoreVideo
import Foundation
import PipelineStandIn
import Testing
import VideoKit

/// The known-color fixtures decoded by VideoToolbox and converted by the
/// renderer's shader must come back as the RGB the generator started from:
/// this checks the matrix, the range, and the NV12 plane handling together.
@Suite struct ColorTests {
    /// COLOR_BARS in fixtures/synthetic/make-video.sh, left to right; each bar
    /// is 32 px wide and 64 px tall.
    static let bars: [SIMD3<Double>] = [
        [0xB4, 0x28, 0x28], [0x28, 0xA0, 0x3C], [0x32, 0x3C, 0xBE],
        [0xDC, 0xDC, 0xDC], [0x80, 0x80, 0x80], [0x18, 0x18, 0x18],
    ]

    /// Largest per-channel error over the bar centers.
    /// `shaderMatrix` replaces the matrix the configuration chose, to check
    /// that a mismatch is caught.
    private func barError(
        _ name: String, color: StandInColor, shaderMatrix: YCbCrToRGB? = nil
    ) throws -> (Double, StandInTile) {
        var playback = try FixturePlayback(name, color: color)
        playback.playAll()
        #expect(playback.log.failures.isEmpty)
        #expect(playback.log.decodedPTS.count == 10)
        let renderer = try GridRenderer()
        renderer.tiles = [GridTile(
            picture: playback.tile.decoder, color: shaderMatrix ?? color.matrix,
            destination: CGRect(x: 0, y: 0, width: 192, height: 96))]
        let image = try #require(renderer.renderOffscreen(width: 192, height: 96))
        var worst = 0.0
        for (i, expected) in Self.bars.enumerated() {
            // An 8x8 patch in the middle of the bar, clear of chroma edges.
            let got = image.mean(CGRect(x: 32 * i + 12, y: 28, width: 8, height: 8))
            let error = maxDifference(got, expected)
            worst = max(worst, error)
        }
        return (worst, playback.tile)
    }

    @Test func bt601VideoRangeH264() throws {
        let (error, tile) = try barError("h264-colors.h264", color: .bt601VideoRange)
        let picture = try #require(tile.decoder.latestPicture())
        #expect(CVPixelBufferGetPixelFormatType(picture.pixelBuffer) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
        #expect(error <= 4, "worst channel error \(error)")
    }

    @Test func bt709FullRangeH265() throws {
        let (error, tile) = try barError("h265-colors.h265", color: .bt709FullRange)
        let picture = try #require(tile.decoder.latestPicture())
        #expect(CVPixelBufferGetPixelFormatType(picture.pixelBuffer) == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)
        #expect(error <= 4, "worst channel error \(error)")
    }

    /// The check is sensitive: a shader matrix for the wrong range or the
    /// wrong coefficients is caught.
    @Test func wrongColorIsCaught() throws {
        let (wrongRange, _) = try barError(
            "h264-colors.h264", color: .bt601VideoRange, shaderMatrix: StandInColor.bt601FullRange.matrix)
        #expect(wrongRange > 12, "worst channel error \(wrongRange)")
        let (wrongMatrix, _) = try barError(
            "h265-colors.h265", color: .bt709FullRange, shaderMatrix: StandInColor.bt601FullRange.matrix)
        #expect(wrongMatrix > 8, "worst channel error \(wrongMatrix)")
    }

    /// VideoToolbox converts to the range the output pixel format asks for,
    /// which is why `TileDecoder.configure` takes `fullRange` from the
    /// pipeline's color: the matrix and the samples must agree.
    @Test func decoderOutputFollowsRequestedRange() throws {
        let (error, tile) = try barError("h264-colors.h264", color: .bt601FullRange)
        let picture = try #require(tile.decoder.latestPicture())
        #expect(CVPixelBufferGetPixelFormatType(picture.pixelBuffer) == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)
        #expect(error <= 4, "worst channel error \(error)")
    }

    @Test func matrixMapsReferenceLevels() {
        // Video range: black (16, 128, 128) -> 0, white (235, 128, 128) -> 1.
        let m = StandInColor.bt709VideoRange.matrix
        func apply(_ y: Float, _ cb: Float, _ cr: Float) -> SIMD3<Float> {
            let v = SIMD4<Float>(y / 255, cb / 255, cr / 255, 1)
            return SIMD3((m.r * v).sum(), (m.g * v).sum(), (m.b * v).sum())
        }
        #expect(maxMagnitude(apply(16, 128, 128)) < 1e-5)
        #expect(maxDifference(apply(235, 128, 128), SIMD3(repeating: 1)) < 1e-5)
        // Full range BT.601 pure red is (76.245, 84.97, 255.5) in Y, Cb, Cr.
        let f = StandInColor.bt601FullRange.matrix
        let v = SIMD4<Float>(76.245 / 255, 84.972 / 255, 255.5 / 255, 1)
        let red = SIMD3((f.r * v).sum(), (f.g * v).sum(), (f.b * v).sum())
        #expect(maxDifference(red, SIMD3(1, 0, 0)) < 1e-3)
        #expect(YCbCrToRGB(rowMajor: Array(repeating: 0, count: 11)) == nil)
    }
}
