import CoreGraphics
import CoreVideo
import Foundation
import FakeCore
import Testing
import VideoKit

/// Fixtures through the stand-in pipeline into a real `TileDecoder`
/// (VideoToolbox), then drawn by a real `GridRenderer` (Metal) offscreen.
@Suite struct TileDecoderTests {
    /// One tile filling the target, at the picture's own size.
    private func render(_ tile: StandInTile, renderer: GridRenderer) throws -> RenderedImage {
        let picture = try #require(tile.decoder.latestPicture())
        renderer.tiles = [
            GridTile(
                picture: tile.decoder, color: try #require(tile.color).matrix,
                destination: CGRect(x: 0, y: 0, width: picture.width, height: picture.height))
        ]
        return try #require(renderer.renderOffscreen(width: picture.width, height: picture.height))
    }

    @Test(arguments: ["h264-basic.h264", "h265-basic.h265"])
    func basic(name: String) throws {
        var playback = try FixturePlayback(name)
        let renderer = try GridRenderer()
        playback.play(through: 9)
        let early = try render(playback.tile, renderer: renderer)
        playback.playAll()
        let late = try render(playback.tile, renderer: renderer)

        #expect(playback.log.failures.isEmpty)
        #expect(playback.log.decodedPTS == (0..<60).map { Int64($0) * 50_000 })
        #expect(playback.tile.gated == 0)
        let stats = playback.tile.decoder.stats
        #expect(stats.sessionsCreated == 1 && stats.liveSessions == 1)
        #expect(stats.decoded == 60 && stats.submitted == 60)

        let picture = try #require(playback.tile.decoder.latestPicture())
        #expect(picture.width == 320 && picture.height == 240)
        #expect(CVPixelBufferGetPixelFormatType(picture.pixelBuffer) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
        #expect(CVPixelBufferGetIOSurface(picture.pixelBuffer) != nil)
        #expect(renderer.pictureSize(ofTile: 0) == CGSize(width: 320, height: 240))

        // Real pixels: a busy test pattern, not a flat or black frame, and it moves.
        #expect(late.spread(late.bounds) > 30)
        #expect(early.mean(early.bounds).sum() > 100)
        #expect(early.bgra != late.bgra)
    }

    @Test(arguments: ["h264-leading-p.h264", "h265-leading-p.h265"])
    func leadingPFramesNeverReachTheDecoder(name: String) throws {
        var playback = try FixturePlayback(name)
        playback.play(through: 18)
        #expect(playback.tile.decoder.latestPicture() == nil)
        #expect(playback.tile.decoder.stats.submitted == 0)
        playback.playAll()
        #expect(playback.log.failures.isEmpty)
        #expect(playback.tile.gated == 19)
        #expect(playback.log.decodedPTS.count == 40)
        let first: Int64? = playback.log.decodedPTS.first
        #expect(first == Int64(19 * 50_000))
        let image = try render(playback.tile, renderer: try GridRenderer())
        #expect(image.spread(image.bounds) > 30)
    }

    @Test(arguments: ["h264-resize.h264", "h265-resize.h265"])
    func resizeReconfiguresAndResizesTheTexture(name: String) throws {
        var playback = try FixturePlayback(name)
        let renderer = try GridRenderer()
        playback.play(through: 39)
        let before = try render(playback.tile, renderer: renderer)
        #expect(renderer.pictureSize(ofTile: 0) == CGSize(width: 320, height: 240))
        #expect(before.width == 320 && before.height == 240)
        playback.playAll()
        let after = try render(playback.tile, renderer: renderer)
        #expect(renderer.pictureSize(ofTile: 0) == CGSize(width: 480, height: 270))
        #expect(after.width == 480 && after.height == 270)
        #expect(after.spread(after.bounds) > 30)

        #expect(playback.log.failures.isEmpty)
        #expect(playback.log.decodedPTS.count == 80)
        let stats = playback.tile.decoder.stats
        #expect(stats.sessionsCreated == 2)
        #expect(stats.sessionsInvalidated == 1)
        #expect(stats.liveSessions == 1)
    }

    @Test func decodeBeforeConfigureFails() throws {
        let log = EventLog()
        let decoder = TileDecoder { log.append($0) }
        decoder.decode(annexB: Data([0, 0, 0, 1, 0x65, 0x88]), keyframe: true, ptsMicros: 0)
        decoder.configure(codec: .h264, parameterSets: [Data([0x67, 0x00])], fullRange: false)
        #expect(log.failures == [.notConfigured, .invalidParameterSets])
        #expect(decoder.latestPicture() == nil)
    }

    @Test func flushDiscardsInFlightAndClearDropsThePicture() throws {
        var playback = try FixturePlayback("h264-basic.h264")
        playback.play(through: 0)
        let decoder = playback.tile.decoder
        #expect(decoder.latestPicture() != nil)

        // Submit a burst without waiting, then flush: whatever had not come
        // out yet is discarded without an event; nothing is lost track of.
        let clip = playback.clip
        for i in 1..<10 {
            decoder.decode(annexB: clip.data.subdata(in: clip.units[i].range), keyframe: false, ptsMicros: Int64(i))
        }
        decoder.flush()
        decoder.waitForPendingFrames()
        let s = decoder.stats
        #expect(s.decoded + s.discarded + s.failed == s.submitted)
        #expect(playback.log.decodedPTS.count == s.decoded)

        let before = decoder.pictureSequence
        decoder.clear()
        #expect(decoder.latestPicture() == nil)
        #expect(decoder.pictureSequence > before)

        // A cleared tile draws black.
        let renderer = try GridRenderer()
        renderer.tiles = [GridTile(
            picture: decoder, color: StandInColor.bt601VideoRange.matrix,
            destination: CGRect(x: 0, y: 0, width: 64, height: 48))]
        let image = try #require(renderer.renderOffscreen(width: 64, height: 48))
        #expect(image.mean(image.bounds) == SIMD3(0, 0, 0))
    }

    /// Reset/Configure cycles release their sessions and pixel buffer pools.
    @Test func resetConfigureCyclesDoNotLeak() throws {
        let clip = try Fixtures.load("h264-basic.h264")
        let key = clip.data.subdata(in: clip.units[0].range)
        let sets = AnnexB.split(key, codec: .h264)
            .filter { NALType.isParameterSet($0.type, codec: .h264) }
            .map { key.subdata(in: $0.range) }
        let log = EventLog()
        let decoder = TileDecoder { log.append($0) }
        let renderer = try GridRenderer()
        renderer.tiles = [GridTile(
            picture: decoder, color: StandInColor.bt601VideoRange.matrix,
            destination: CGRect(x: 0, y: 0, width: 32, height: 24))]

        func cycle(_ n: Int) {
            for i in 0..<n {
                decoder.reset()
                decoder.configure(codec: .h264, parameterSets: sets, fullRange: i % 2 == 0)
                decoder.decode(annexB: key, keyframe: true, ptsMicros: Int64(i))
                decoder.waitForPendingFrames()
                _ = renderer.renderOffscreen(width: 32, height: 24)
            }
        }
        cycle(50)
        let warm = physicalFootprint()
        cycle(200)
        let growth = physicalFootprint() - warm

        let s = decoder.stats
        #expect(log.failures.isEmpty)
        #expect(s.sessionsCreated == 250)
        #expect(s.liveSessions == 1)
        #expect(s.decoded == 250)
        // A coarse guard against unbounded growth (held pictures, textures,
        // pools). It is not sensitive to a leaked session by itself (one
        // grew it by about 17 KB); SessionLifetimeTests checks that directly.
        print("footprint growth over 200 reset/configure cycles: \(growth / 1024) KiB")
        #expect(growth < 8 << 20)
    }
}
