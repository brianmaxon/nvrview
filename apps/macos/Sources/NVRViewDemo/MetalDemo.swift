import AppKit
import PipelineStandIn
import VideoKit

/// Feeds one file through the stand-in pipeline into a `TileDecoder` from a
/// background task, paced like a live channel.
final class MetalFilePlayer: Sendable {
    let tile: StandInTile
    let name: String
    private let task: Task<Void, Never>

    init?(file: URL, fps: Double) {
        let codec: VideoCodec = file.pathExtension == "h265" || file.pathExtension == "hevc" ? .h265 : .h264
        guard let data = try? Data(contentsOf: file) else { return nil }
        let units = AccessUnitSplitter.split(data, codec: codec)
        guard !units.isEmpty else { return nil }
        let tile = StandInTile(codec: codec)
        self.tile = tile
        name = file.lastPathComponent
        task = Task.detached {
            let clock = ContinuousClock()
            let period = Duration.seconds(1.0 / fps)
            var next = clock.now
            var n = 0
            while !Task.isCancelled {
                let unit = units[n % units.count]
                tile.push(data.subdata(in: unit.range), ptsMicros: Int64(Double(n) * 1_000_000 / fps))
                n += 1
                next += period
                try? await clock.sleep(until: next)
            }
        }
    }

    deinit { task.cancel() }
}

/// The grid view plus the layout bookkeeping the stand-in does each refresh:
/// recompute rectangles only when the drawable size, a picture size, or a
/// color changes.
@MainActor
final class MetalGrid {
    static let gap: CGFloat = 4

    let players: [MetalFilePlayer]
    let view: MetalGridView
    private var drawableSize = CGSize.zero
    private var sizes: [CGSize?]
    private var colors: [StandInColor?]

    init(players: [MetalFilePlayer]) throws {
        self.players = players
        view = MetalGridView(renderer: try GridRenderer())
        sizes = Array(repeating: nil, count: players.count)
        colors = Array(repeating: nil, count: players.count)
        view.prepare = { [unowned self] renderer, size in self.update(renderer, size) }
    }

    private func update(_ renderer: GridRenderer, _ size: CGSize) {
        var changed = size != drawableSize || renderer.tiles.count != players.count
        for (i, player) in players.enumerated() {
            let picture = player.tile.decoder.latestPicture()
            let pictureSize = picture.map { CGSize(width: $0.width, height: $0.height) }
            if pictureSize != sizes[i] && pictureSize != nil {
                sizes[i] = pictureSize
                changed = true
            }
            let color = player.tile.color
            if color != colors[i] {
                colors[i] = color
                changed = true
            }
        }
        guard changed else { return }
        drawableSize = size
        let rects = StandInLayout.grid(count: players.count, viewport: size, gap: Self.gap, pictureSizes: sizes)
        renderer.tiles = players.indices.map { i in
            GridTile(
                picture: players[i].tile.decoder,
                color: (colors[i] ?? StandInColor.bt601VideoRange).matrix,
                destination: rects[i])
        }
    }
}

/// Plays the Metal grid for a fixed time, reading back exactly the frame
/// presented at the midpoint and at the end, and checks every tile:
///
/// - its picture fills its rectangle with the picture's aspect ratio, with
///   black just outside;
/// - the picture is not flat and differs from at least one of two earlier
///   readbacks taken 0.12 s apart (so a short looping clip cannot line up
///   with both);
/// - pictures kept decoding, with no decode failures.
///
/// Prints one line per tile and a verdict, then exits 0 (pass) or 1 (fail).
@MainActor
final class MetalSelfTest {
    private let grid: MetalGrid
    private let seconds: Double
    private var earlier: [(image: RenderedImage, tiles: [GridTile], decoded: [Int])] = []
    private var finished = false

    init(grid: MetalGrid, seconds: Double) {
        self.grid = grid
        self.seconds = seconds
    }

    func start(window: NSWindow) {
        for delay in [seconds / 2, seconds / 2 + 0.12] {
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [self] in
                grid.view.captureNextFrame { [self] image in
                    earlier.append((image, grid.view.renderer.tiles, decodedCounts()))
                }
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [self] in
            grid.view.captureNextFrame { [self] image in finish(window: window, image: image) }
        }
        // If the view never draws (not on screen), fail instead of hanging.
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds + 5) { [self] in
            if !finished {
                print("selftest: no frame was drawn")
                print("selftest: FAIL")
                exit(1)
            }
        }
    }

    private func decodedCounts() -> [Int] { grid.players.map { $0.tile.decoder.stats.decoded } }

    private func finish(window: NSWindow, image: RenderedImage) {
        finished = true
        var failures: [String] = []
        let content = window.contentLayoutRect.size
        let onScreen = window.isVisible && window.screen != nil
        print(String(
            format: "window: %.0fx%.0f, drawable: %dx%d, on screen: %@, refreshes %d, drawn %d",
            content.width, content.height, image.width, image.height, onScreen ? "yes" : "no",
            grid.view.refreshes, grid.view.framesDrawn))
        if !onScreen { failures.append("window is not on screen") }
        if content.width < 480 || content.height < 360 { failures.append("window is smaller than 480x360") }
        guard earlier.count == 2, let midpoint = earlier.first else {
            print("  the midpoint frames were not captured")
            print("selftest: FAIL")
            exit(1)
        }

        let tiles = grid.view.renderer.tiles
        let decoded = decodedCounts()
        for (i, player) in grid.players.enumerated() {
            var problems: [String] = []
            let rect = i < tiles.count ? tiles[i].destination : .zero
            let size = grid.view.renderer.pictureSize(ofTile: i)
            if let size, rect.width > 0, rect.height > 0 {
                let aspect = (rect.width / rect.height) / (size.width / size.height)
                if abs(aspect - 1) > 0.02 { problems.append("rectangle \(rect) does not match a \(size) picture") }
                problems += Self.edgeProblems(image, rect, others: tiles.map(\.destination))
            } else {
                problems.append("no picture")
            }
            let spread = Self.spread(image, rect)
            if spread < 8 { problems.append(String(format: "picture is flat (spread %.1f)", spread)) }
            let now = Self.region(image, rect)
            let unchanged = earlier.allSatisfy { e in
                i < e.tiles.count && e.tiles[i].destination == rect && Self.region(e.image, rect) == now
            }
            if unchanged { problems.append("picture did not change") }
            if decoded[i] <= midpoint.decoded[i] { problems.append("decoding stopped") }
            if player.tile.decodeFailures > 0 { problems.append("decode failures: \(player.tile.decodeFailures)") }
            let s = player.tile.decoder.stats
            print(
                "tile \(i) \(player.name): rect \(Int(rect.minX)),\(Int(rect.minY)) \(Int(rect.width))x\(Int(rect.height)), "
                    + "picture \(size.map { "\(Int($0.width))x\(Int($0.height))" } ?? "none"), "
                    + "decoded \(s.decoded), gated \(player.tile.gated), sessions \(s.sessionsCreated), "
                    + String(format: "spread %.1f ", spread)
                    + (problems.isEmpty ? "OK" : "FAIL"))
            failures += problems.map { "tile \(i): \($0)" }
        }

        if failures.isEmpty {
            print("selftest: PASS")
            exit(0)
        }
        for f in failures { print("  \(f)") }
        print("selftest: FAIL")
        exit(1)
    }

    /// The picture reaches every edge of its rectangle (some lit pixels on
    /// each edge) and nothing is drawn just outside it.
    private static func edgeProblems(_ image: RenderedImage, _ r: CGRect, others: [CGRect]) -> [String] {
        let x0 = Int(r.minX), y0 = Int(r.minY), x1 = Int(r.maxX) - 1, y1 = Int(r.maxY) - 1
        guard x0 >= 0, y0 >= 0, x1 < image.width, y1 < image.height, x1 > x0, y1 > y0 else {
            return ["rectangle \(r) is outside the \(image.width)x\(image.height) frame"]
        }
        func lit(_ x: Int, _ y: Int) -> Bool { image.rgb(x: x, y: y).wrappedSum() > 24 }
        var problems: [String] = []
        let edges: [(String, [(Int, Int)])] = [
            ("top", (x0...x1).map { ($0, y0) }), ("bottom", (x0...x1).map { ($0, y1) }),
            ("left", (y0...y1).map { (x0, $0) }), ("right", (y0...y1).map { (x1, $0) }),
        ]
        for (name, points) in edges where points.filter({ lit($0.0, $0.1) }).count * 4 < points.count {
            problems.append("picture does not reach the \(name) edge")
        }
        let outside = [(x0 - 1, (y0 + y1) / 2), (x1 + 1, (y0 + y1) / 2), ((x0 + x1) / 2, y0 - 1), ((x0 + x1) / 2, y1 + 1)]
        for (x, y) in outside where x >= 0 && y >= 0 && x < image.width && y < image.height {
            let p = CGPoint(x: CGFloat(x) + 0.5, y: CGFloat(y) + 0.5)
            if !others.contains(where: { $0.contains(p) }) && image.rgb(x: x, y: y) != SIMD3(0, 0, 0) {
                problems.append("drawn outside its rectangle at \(x),\(y)")
            }
        }
        return problems
    }

    private static func region(_ image: RenderedImage, _ r: CGRect) -> [UInt8] {
        guard r.width > 0, r.height > 0 else { return [] }
        var out: [UInt8] = []
        for y in Int(r.minY)..<min(Int(r.maxY), image.height) {
            let start = (y * image.width + Int(r.minX)) * 4
            let end = (y * image.width + min(Int(r.maxX), image.width)) * 4
            out += image.bgra[start..<end]
        }
        return out
    }

    /// Standard deviation of brightness over a rectangle, sampled every 2 px.
    private static func spread(_ image: RenderedImage, _ r: CGRect) -> Double {
        guard r.width > 0, r.height > 0 else { return 0 }
        var sum = 0.0, sumSq = 0.0, n = 0.0
        for y in stride(from: Int(r.minY), to: min(Int(r.maxY), image.height), by: 2) {
            for x in stride(from: Int(r.minX), to: min(Int(r.maxX), image.width), by: 2) {
                let v = Double(image.rgb(x: x, y: y).wrappedSum()) / 3
                sum += v
                sumSq += v * v
                n += 1
            }
        }
        guard n > 0 else { return 0 }
        let mean = sum / n
        return max(sumSq / n - mean * mean, 0).squareRoot()
    }
}
