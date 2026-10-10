import CoreGraphics
import CoreVideo
import FakeCore
import Foundation
import NVRCore
import Testing
import VideoKit

/// The Metal dewarp pipeline against the pure reference: a synthetic fisheye
/// picture whose four quadrants (around the circle's center) have known
/// colors, drawn in every mode and mount; each output pixel clear of a
/// boundary must show the quadrant the reference says it samples, or black.
@Suite struct FisheyeRenderTests {
    private static let circle = FisheyeCircle(cx: 0.49, cy: 0.505, r: 0.46)
    private static let size = 256
    /// Video-range BT.601 Y, Cb, Cr and the RGB they decode to.
    private static let quadrants: [(y: UInt8, cb: UInt8, cr: UInt8, rgb: SIMD3<Int>)] = [
        (82, 90, 240, [255, 0, 0]),  // right, above center: red
        (145, 54, 34, [0, 255, 0]),  // left, above: green
        (41, 240, 110, [0, 0, 255]),  // left, below: blue
        (210, 16, 146, [255, 255, 0]),  // right, below: yellow
    ]

    /// Which quadrant a picture point is in, or nil outside the circle.
    private static func quadrant(_ p: CGPoint) -> Int? {
        let dx = Double(p.x) - circle.cx, dy = Double(p.y) - circle.cy
        guard (dx * dx + dy * dy).squareRoot() <= circle.r else { return nil }
        return dy < 0 ? (dx >= 0 ? 0 : 1) : (dx < 0 ? 2 : 3)
    }

    /// Distance in picture pixels from the nearest color boundary (the two
    /// axes through the center and the rim).
    private static func clearance(_ p: CGPoint) -> Double {
        let s = Double(size)
        let dx = (Double(p.x) - circle.cx) * s, dy = (Double(p.y) - circle.cy) * s
        return min(abs(dx), abs(dy), abs((dx * dx + dy * dy).squareRoot() - circle.r * s))
    }

    private static func picture() -> DecodedPicture {
        let attributes: [CFString: Any] = [
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any](),
        ]
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(
            kCFAllocatorDefault, size, size, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            attributes as CFDictionary, &buffer)
        let pb = buffer!
        CVPixelBufferLockBaseAddress(pb, [])
        let luma = CVPixelBufferGetBaseAddressOfPlane(pb, 0)!.assumingMemoryBound(to: UInt8.self)
        let ls = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
        let chroma = CVPixelBufferGetBaseAddressOfPlane(pb, 1)!.assumingMemoryBound(to: UInt8.self)
        let cs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
        for y in 0..<size {
            for x in 0..<size {
                let p = CGPoint(x: (Double(x) + 0.5) / Double(size), y: (Double(y) + 0.5) / Double(size))
                let q = quadrant(p).map { quadrants[$0] } ?? (16, 128, 128, [0, 0, 0])
                luma[y * ls + x] = q.y
                if x % 2 == 0 && y % 2 == 0 {
                    chroma[(y / 2) * cs + x] = q.cb
                    chroma[(y / 2) * cs + x + 1] = q.cr
                }
            }
        }
        CVPixelBufferUnlockBaseAddress(pb, [])
        return DecodedPicture(pixelBuffer: pb, ptsMicros: 0, sequence: 1)
    }

    private final class Still: TilePictureSource {
        let picture: DecodedPicture
        init(_ picture: DecodedPicture) { self.picture = picture }
        func latestPicture() -> DecodedPicture? { picture }
        var pictureSequence: UInt64 { picture.sequence }
    }

    @Test func everyModeAndMountDrawsWhatTheReferenceSays() throws {
        let renderer = try GridRenderer()
        let still = Still(Self.picture())
        let color = StandInColor.bt601VideoRange.matrix
        let tile = CGRect(x: 0, y: 0, width: 320, height: 180)
        let lens = FisheyeLens(circle: Self.circle)
        var checked = 0
        for mount in FisheyeMount.allCases {
            for mode in FisheyeViewMode.allCases where mode.dewarps {
                let aim = FisheyeAim(pan: 25, tilt: mount == .desk ? -35 : 35, fov: 80)
                let dewarp = FisheyeDewarp(lens: lens, mount: mount, mode: mode, aim: aim, tileSize: tile.size)
                renderer.tiles = [GridTile(picture: still, color: color, destination: tile, dewarp: dewarp.constants)]
                let image = try #require(renderer.renderOffscreen(width: 320, height: 180))
                #expect(renderer.dewarpedTilesDrawn == 1)
                var mismatches = 0
                for y in stride(from: 2, to: 178, by: 7) {
                    for x in stride(from: 2, to: 318, by: 7) {
                        let t = CGPoint(x: (Double(x) + 0.5) / 320, y: (Double(y) + 0.5) / 180)
                        // Stay clear of view boundaries (tile pixels) too.
                        guard dewarp.views.allSatisfy({ v in
                            let r = v.rect
                            return min(abs(Double(t.x - r.minX)), abs(Double(t.x - r.maxX))) * 320 > 2
                                && min(abs(Double(t.y - r.minY)), abs(Double(t.y - r.maxY))) * 180 > 2
                                || !r.insetBy(dx: -0.01, dy: -0.01).contains(t)
                        }) else { continue }
                        let want: SIMD3<Int>
                        if let p = dewarp.picturePoint(atTile: t) {
                            guard Self.clearance(p) > 4 else { continue }
                            want = Self.quadrant(p).map { Self.quadrants[$0].rgb } ?? [0, 0, 0]
                        } else {
                            want = [0, 0, 0]
                        }
                        let got = image.rgb(x: x, y: y)
                        if abs(got.x - want.x) > 24 || abs(got.y - want.y) > 24 || abs(got.z - want.z) > 24 {
                            mismatches += 1
                        }
                        checked += 1
                    }
                }
                #expect(mismatches == 0, "\(mount) \(mode): \(mismatches) pixels differ from the reference")
            }
        }
        #expect(checked > 2000)
    }

    @Test func originalTilesStayOnTheOrdinaryPipeline() throws {
        let renderer = try GridRenderer()
        let still = Still(Self.picture())
        renderer.tiles = [
            GridTile(
                picture: still, color: StandInColor.bt601VideoRange.matrix,
                destination: CGRect(x: 0, y: 0, width: 64, height: 64))
        ]
        let image = try #require(renderer.renderOffscreen(width: 64, height: 64))
        #expect(renderer.dewarpedTilesDrawn == 0)
        // The circle as it is: top right red, bottom left blue.
        #expect(image.rgb(x: 48, y: 16).x > 200 && image.rgb(x: 16, y: 48).z > 200)
    }
}
