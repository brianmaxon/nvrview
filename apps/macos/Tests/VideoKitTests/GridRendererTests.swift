import CoreGraphics
import Foundation
import PipelineStandIn
import Testing
import VideoKit

@Suite struct GridRendererTests {
    private let color = StandInColor.bt601VideoRange.matrix

    /// Video-range Y, Cb, Cr for a few saturated colors (BT.601).
    private static let swatches: [(y: UInt8, cb: UInt8, cr: UInt8, rgb: SIMD3<Double>)] = [
        (82, 90, 240, [255, 0, 0]),
        (145, 54, 34, [0, 255, 0]),
        (41, 240, 110, [0, 0, 255]),
        (235, 128, 128, [255, 255, 255]),
        (210, 16, 146, [255, 255, 0]),
    ]

    @Test func drawsEachTileIntoItsRectangleWithBlackAround() throws {
        let renderer = try GridRenderer()
        let viewport = CGSize(width: 400, height: 300)
        // Mixed aspect ratios, so some tiles get bars on the sides, some on top.
        let sizes = [CGSize(width: 64, height: 48), CGSize(width: 64, height: 36), CGSize(width: 48, height: 64),
                     CGSize(width: 64, height: 48), CGSize(width: 32, height: 32)]
        let rects = StandInLayout.grid(count: 5, viewport: viewport, gap: 4, pictureSizes: sizes)
        renderer.tiles = zip(Self.swatches, zip(sizes, rects)).map { swatch, sr in
            let still = StillPicture.flat(
                width: Int(sr.0.width), height: Int(sr.0.height), y: swatch.y, cb: swatch.cb, cr: swatch.cr)
            return GridTile(picture: still, color: color, destination: sr.1)
        }
        let image = try #require(renderer.renderOffscreen(width: 400, height: 300))

        var covered = 0
        for (swatch, rect) in zip(Self.swatches, rects) {
            // Inside (one pixel in from the edges, to stay clear of blending): the tile's color.
            let inside = rect.insetBy(dx: 1, dy: 1)
            #expect(maxDifference(image.mean(inside), swatch.rgb) < 4, "tile at \(rect)")
            #expect(image.spread(inside) < 1)
            covered += Int(rect.width * rect.height)
            // Just outside, inside the image: black.
            for p in [CGPoint(x: rect.minX - 1, y: rect.midY), CGPoint(x: rect.maxX, y: rect.midY),
                      CGPoint(x: rect.midX, y: rect.minY - 1), CGPoint(x: rect.midX, y: rect.maxY)]
            where image.bounds.contains(p) && !rects.contains(where: { $0.contains(p) }) {
                #expect(image.rgb(x: Int(p.x), y: Int(p.y)) == SIMD3(0, 0, 0), "outside \(rect) at \(p)")
            }
        }
        // Everything outside the tiles is black: count non-black pixels.
        var lit = 0
        for y in 0..<300 {
            for x in 0..<400 where image.rgb(x: x, y: y) != SIMD3(0, 0, 0) { lit += 1 }
        }
        #expect(lit == covered)
    }

    @Test func sourceRectangleCrops() throws {
        // Left half red, right half blue; draw only the right half.
        let picture = DecodedPicture.nv12(width: 64, height: 32) { x, _ in
            x < 32 ? (82, 90, 240) : (41, 240, 110)
        }
        let renderer = try GridRenderer()
        renderer.tiles = [GridTile(
            picture: StillPicture(picture), color: color,
            destination: CGRect(x: 0, y: 0, width: 64, height: 64),
            source: CGRect(x: 32, y: 0, width: 32, height: 32))]
        let image = try #require(renderer.renderOffscreen(width: 64, height: 64))
        #expect(maxDifference(image.mean(CGRect(x: 2, y: 2, width: 60, height: 60)), SIMD3(0, 0, 255)) < 4)
    }

    @Test func needsDisplayOnlyWhenSomethingChanged() throws {
        var playback = try FixturePlayback("h264-basic.h264")
        playback.play(through: 0)
        let renderer = try GridRenderer()
        #expect(renderer.needsDisplay(width: 64, height: 48))
        renderer.tiles = [GridTile(
            picture: playback.tile.decoder, color: color,
            destination: CGRect(x: 0, y: 0, width: 64, height: 48))]
        _ = renderer.renderOffscreen(width: 64, height: 48)
        #expect(!renderer.needsDisplay(width: 64, height: 48))
        #expect(renderer.needsDisplay(width: 80, height: 48))
        playback.play(through: 1)
        #expect(renderer.needsDisplay(width: 64, height: 48))
        _ = renderer.renderOffscreen(width: 64, height: 48)
        #expect(!renderer.needsDisplay(width: 64, height: 48))
        playback.tile.decoder.clear()
        #expect(renderer.needsDisplay(width: 64, height: 48))
    }

    @Test func layoutFitsAndCenters() {
        let rects = StandInLayout.grid(
            count: 2, viewport: CGSize(width: 202, height: 100), gap: 2,
            pictureSizes: [CGSize(width: 320, height: 240), nil])
        #expect(rects == [CGRect(x: 0, y: 13, width: 100, height: 75), CGRect(x: 102, y: 0, width: 100, height: 100)])
    }

    /// Steady-state cost of a 6-tile frame (printed, loosely bounded).
    @Test func sixTileFrameTime() throws {
        var players: [FixturePlayback] = []
        for name in ["h264-basic.h264", "h265-basic.h265", "h264-resize.h264",
                     "h265-resize.h265", "h264-colors.h264", "h265-colors.h265"] {
            var p = try FixturePlayback(name)
            p.play(through: 5)
            players.append(p)
        }
        let renderer = try GridRenderer()
        let rects = StandInLayout.grid(
            count: 6, viewport: CGSize(width: 1920, height: 1080), gap: 2,
            pictureSizes: players.map { $0.tile.decoder.latestPicture().map { CGSize(width: $0.width, height: $0.height) } })
        renderer.tiles = zip(players, rects).map {
            GridTile(picture: $0.tile.decoder, color: $0.tile.color!.matrix, destination: $1)
        }
        _ = renderer.renderOffscreen(width: 1920, height: 1080)
        let clock = ContinuousClock()
        let n = 30
        let elapsed = clock.measure {
            for _ in 0..<n { _ = renderer.renderOffscreen(width: 1920, height: 1080) }
        }
        let ms = Double(elapsed.components.attoseconds) / 1e15 / Double(n) + Double(elapsed.components.seconds) * 1000 / Double(n)
        print(String(format: "6 tiles at 1920x1080, offscreen render + readback: %.2f ms per frame", ms))
        #expect(ms < 100)
    }
}
