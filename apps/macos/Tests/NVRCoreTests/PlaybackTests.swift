import CoreGraphics
import FakeCore
import Foundation
import NVRCore
import Testing
import VideoKit

/// Fixtures played by `FakeCore`, executed by `CoreBridge` into a real
/// `TileDecoder` (VideoToolbox), drawn by a real `GridRenderer` (Metal)
/// offscreen: the whole path a live channel takes, minus the Rust core.
@Suite struct PlaybackTests {
    private func render(_ rig: Rig, _ tile: CoreTile, viewport: CGSize, renderer: GridRenderer) throws -> RenderedImage {
        let grid = CoreGrid(bridge: rig.bridge, gap: 0)
        grid.update(renderer: renderer, tiles: [tile], viewport: viewport)
        return try #require(renderer.renderOffscreen(width: Int(viewport.width), height: Int(viewport.height)))
    }

    private func expectClean(_ rig: Rig) async {
        #expect(await rig.fake.waitUntilAnswered())
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
        #expect(rig.fake.answersAfterFree == 0)
    }

    @Test(arguments: ["h264-basic.h264", "h265-basic.h265"])
    func basic(name: String) async throws {
        let rig = try Rig(try Fixtures.clip(name))
        let tile = try rig.open()
        let renderer = try GridRenderer()
        rig.play(tile, 10)
        let early = try render(rig, tile, viewport: CGSize(width: 320, height: 240), renderer: renderer)
        rig.play(tile, 50)
        let late = try render(rig, tile, viewport: CGSize(width: 320, height: 240), renderer: renderer)
        await expectClean(rig)

        let answers = try #require(rig.fake.answers(tile: tile.id))
        #expect(answers.failures.isEmpty)
        #expect(answers.decodedPTS == (0..<60).map { Int64($0) * 50_000 })
        #expect(rig.fake.gated(tile: tile.id) == 0)
        let snapshot = tile.snapshot
        #expect(snapshot.codedSize == PixelSize(width: 320, height: 240))
        #expect(snapshot.visibleRect == PixelRect(x: 0, y: 0, width: 320, height: 240))
        #expect(snapshot.configures == 1)
        #expect(tile.decoder.stats.sessionsCreated == 1)
        #expect(snapshot.status == .playing)

        #expect(late.spread(late.bounds) > 30)
        #expect(early.mean(early.bounds).sum() > 100)
        #expect(early.bgra != late.bgra)
        rig.bridge.shutdown()
    }

    /// Color description and range cross the contract intact: the decoder
    /// outputs the configured range and the shader gets the configured matrix.
    @Test(arguments: [
        ("h264-colors.h264", StandInColor.bt601VideoRange), ("h265-colors.h265", StandInColor.bt709FullRange),
    ])
    func colorsSurviveTheBridge(name: String, color: StandInColor) async throws {
        let rig = try Rig(try Fixtures.clip(name, color: color))
        let tile = try rig.open()
        rig.play(tile, 10)
        await expectClean(rig)
        let snapshot = tile.snapshot
        #expect(snapshot.color?.fullRange == color.fullRange)
        #expect(snapshot.color?.matrix == (color == .bt601VideoRange ? .bt601 : .bt709))
        let renderer = try GridRenderer()
        let image = try render(rig, tile, viewport: CGSize(width: 192, height: 96), renderer: renderer)
        let bars: [SIMD3<Double>] = [
            [0xB4, 0x28, 0x28], [0x28, 0xA0, 0x3C], [0x32, 0x3C, 0xBE],
            [0xDC, 0xDC, 0xDC], [0x80, 0x80, 0x80], [0x18, 0x18, 0x18],
        ]
        for (i, expected) in bars.enumerated() {
            let got = image.mean(CGRect(x: 32 * i + 12, y: 28, width: 8, height: 8))
            #expect(maxMagnitude(got - expected) <= 4, "bar \(i): \(got) vs \(expected)")
        }
        rig.bridge.shutdown()
    }

    @Test(arguments: ["h264-resize.h264", "h265-resize.h265"])
    func resize(name: String) async throws {
        let rig = try Rig(try Fixtures.clip(name))
        let tile = try rig.open()
        let renderer = try GridRenderer()
        rig.play(tile, 40)
        let small = try render(rig, tile, viewport: CGSize(width: 320, height: 240), renderer: renderer)
        #expect(tile.snapshot.codedSize == PixelSize(width: 320, height: 240))
        #expect(small.spread(small.bounds) > 30)
        rig.play(tile, 40)
        await expectClean(rig)
        let big = try render(rig, tile, viewport: CGSize(width: 480, height: 270), renderer: renderer)
        let snapshot = tile.snapshot
        #expect(snapshot.codedSize == PixelSize(width: 480, height: 270))
        #expect(snapshot.configures == 2)
        let picture = try #require(tile.decoder.latestPicture())
        #expect(picture.width == 480 && picture.height == 270)
        let stats = tile.decoder.stats
        #expect(stats.sessionsCreated == 2 && stats.liveSessions == 1)
        // The new size fills the viewport exactly: the layout followed the Configure.
        let drawn = try #require(renderer.tiles.first?.destination)
        #expect(drawn == CGRect(x: 0, y: 0, width: 480, height: 270))
        #expect(big.spread(big.bounds) > 30)
        let answers = try #require(rig.fake.answers(tile: tile.id))
        #expect(answers.failures.isEmpty && answers.decodedPTS.count == 80)
        rig.bridge.shutdown()
    }

    @Test(arguments: ["h264-leading-p.h264", "h265-leading-p.h265"])
    func leadingPFramesAreGated(name: String) async throws {
        let rig = try Rig(try Fixtures.clip(name))
        let tile = try rig.open()
        rig.play(tile, 19)
        #expect(tile.decoder.latestPicture() == nil)
        #expect(tile.decoder.stats.submitted == 0)
        #expect(tile.snapshot.configures == 0)
        rig.play(tile, 40)
        await expectClean(rig)
        #expect(rig.fake.gated(tile: tile.id) == 19)
        let answers = try #require(rig.fake.answers(tile: tile.id))
        #expect(answers.failures.isEmpty && answers.decodedPTS.count == 40)
        let first: Int64? = answers.decodedPTS.first
        #expect(first == 950_000)
        rig.bridge.shutdown()
    }

    /// Several channels of one device, each with its own decoder, through one grid.
    @Test func gridOfTiles() async throws {
        var options = FakeCore.Options(clips: [
            try Fixtures.clip("h264-basic.h264"), try Fixtures.clip("h265-basic.h265"),
            try Fixtures.clip("h264-resize.h264"),
        ])
        options.loop = false
        let rig = try Rig(options)
        let tiles = try (0..<3).map { try rig.open(channel: UInt8($0)) }
        for tile in tiles { rig.play(tile, 30) }
        await expectClean(rig)
        let renderer = try GridRenderer()
        let grid = CoreGrid(bridge: rig.bridge, gap: 4)
        #expect(grid.update(renderer: renderer, tiles: tiles, viewport: CGSize(width: 640, height: 480)))
        #expect(!grid.update(renderer: renderer, tiles: tiles, viewport: CGSize(width: 640, height: 480)))
        let image = try #require(renderer.renderOffscreen(width: 640, height: 480))
        for tile in renderer.tiles {
            let r = tile.destination
            #expect(r.width > 0 && r.height > 0)
            #expect(image.spread(r.insetBy(dx: 2, dy: 2)) > 30, "tile at \(r)")
        }
        rig.bridge.shutdown()
    }

    /// If the buffer is the coded size and the core's visible rectangle is
    /// smaller, source rectangles are offset by its origin: the cropped tile
    /// shows the same pixels as that region of the full picture.
    @Test func visibleRectangleOffsetsTheSource() async throws {
        let clip = try Fixtures.clip("h264-basic.h264")
        let rig = try Rig(clip)
        let tile = try rig.open()
        let visible = PixelRect(x: 16, y: 16, width: 288, height: 208)
        rig.fake.inject(
            tile: tile.id,
            .configure(
                codec: .h264, parameterSets: Fixtures.parameterSets(clip), color: .bt601VideoRange,
                visible: visible))
        rig.fake.inject(tile: tile.id, .decode(Fixtures.accessUnit(clip, 0), keyframe: true, ptsMicros: 0))
        tile.decoder.waitForPendingFrames()
        await expectClean(rig)
        #expect(tile.snapshot.visibleRect == visible)

        let renderer = try GridRenderer()
        let full = try render(rig, tile, viewport: CGSize(width: 288, height: 208), renderer: renderer)
        // Layout speaks in visible pixels: a 288x208 viewport is 1:1.
        let gridTile = try #require(renderer.tiles.first)
        #expect(gridTile.destination == CGRect(x: 0, y: 0, width: 288, height: 208))
        #expect(gridTile.source == CGRect(x: 16, y: 16, width: 288, height: 208))

        // Same picture, no crop: render the whole 320x240 buffer for reference.
        let reference = GridTile(
            picture: tile.decoder, color: gridTile.color, destination: CGRect(x: 0, y: 0, width: 320, height: 240))
        renderer.tiles = [reference]
        let whole = try #require(renderer.renderOffscreen(width: 320, height: 240))
        var worst = 0
        for y in stride(from: 0, to: 208, by: 7) {
            for x in stride(from: 0, to: 288, by: 7) {
                let a = full.rgb(x: x, y: y), b = whole.rgb(x: x + 16, y: y + 16)
                worst = max(worst, abs(a.x - b.x), abs(a.y - b.y), abs(a.z - b.z))
            }
        }
        #expect(worst <= 3, "cropped tile differs from the full picture by \(worst)")
        rig.bridge.shutdown()
    }

    @Test func pictureMappingRules() {
        var s = TileSnapshot()
        s.codedSize = PixelSize(width: 1920, height: 1088)
        s.visibleRect = PixelRect(x: 0, y: 4, width: 1920, height: 1080)
        s.color = CoreColor(matrix: .bt709, fullRange: false, ycbcrToRGB: Array(repeating: 0, count: 12))
        let visible = PixelSize(width: 1920, height: 1080)
        // Cropped buffer: as is.
        #expect(PictureMapping(snapshot: s, pictureSize: visible) == PictureMapping(layoutSize: visible, x: 0, y: 0))
        // Full coded buffer: offset by the visible origin.
        #expect(
            PictureMapping(snapshot: s, pictureSize: s.codedSize)
                == PictureMapping(layoutSize: visible, x: 0, y: 4))
        // A stale picture of another size: its own size, no offset.
        let stale = PixelSize(width: 320, height: 240)
        #expect(PictureMapping(snapshot: s, pictureSize: stale) == PictureMapping(layoutSize: stale, x: 0, y: 0))
        // No picture: nothing to lay out.
        #expect(PictureMapping(snapshot: s, pictureSize: nil) == PictureMapping(layoutSize: .zero, x: 0, y: 0))
    }
}
