import AppKit
import FakeCore
import Foundation
import NVRCore
import VideoKit

/// The Metal demo's core: a `FakeCore` playing the given files (one per
/// channel, round-robin) behind a real `CoreBridge`, paced like live channels.
/// One device, one tile per channel; the bridge executes the core's actions
/// with `TileDecoder`, exactly as it will for the Rust core.
final class DemoCore: Sendable {
    let bridge: CoreBridge
    let fake: FakeCore
    let tiles: [CoreTile]
    let names: [String]

    init?(files: [URL], tiles count: Int, fps: Double) {
        guard let clips = try? files.map({ try FakeClip.load($0) }), clips.allSatisfy({ !$0.units.isEmpty }) else {
            return nil
        }
        var options = FakeCore.Options(clips: clips)
        options.pacing = .realtime(fps: fps)
        options.channelCount = count
        let box = FakeCoreBox()
        let bridge: CoreBridge
        do {
            bridge = try CoreBridge(factory: FakeCore.factory(options: options) { box.set($0) })
            guard let fake = box.value else { return nil }
            self.fake = fake
            let device = try bridge.addDevice(host: "192.0.2.1", port: 9000, user: "example-user", password: "example")
            var opened: [CoreTile] = []
            for channel in 0..<count {
                opened.append(try bridge.openTile(device: device, channel: UInt8(channel), tier: .main))
            }
            self.tiles = opened
        } catch {
            return nil
        }
        self.bridge = bridge
        names = (0..<count).map { clips[$0 % clips.count].name }
    }

    /// Closes the core (every tile, then the core itself).
    func shutdown() { bridge.shutdown() }
}

/// Hands the `FakeCore` the factory creates back to the caller.
final class FakeCoreBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: FakeCore?
    func set(_ core: FakeCore) { lock.lock(); stored = core; lock.unlock() }
    var value: FakeCore? { lock.lock(); defer { lock.unlock() }; return stored }
}

/// The grid view plus the layout bookkeeping done each refresh: the core's
/// layout is recomputed only when the drawable size, a picture size, or a
/// tile's configuration changes.
@MainActor
final class MetalGrid {
    let core: DemoCore
    let view: MetalGridView
    private let layout: CoreGrid

    init(core: DemoCore) throws {
        self.core = core
        view = MetalGridView(renderer: try GridRenderer())
        layout = CoreGrid(bridge: core.bridge, gap: 4)
        let tiles = core.tiles
        view.prepare = { [layout] renderer, size in
            layout.update(renderer: renderer, tiles: tiles, viewport: size)
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
    private let untilFrames: Int?
    private var earlier: [(image: RenderedImage, tiles: [GridTile], decoded: [Int])] = []
    private var finished = false

    init(grid: MetalGrid, seconds: Double, untilFrames: Int? = nil) {
        self.grid = grid
        self.seconds = seconds
        self.untilFrames = untilFrames
    }

    func start(window: NSWindow) {
        if let frames = untilFrames {
            startUntilFrames(window: window, frames: frames)
            return
        }
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
        failIfNoFrame(after: seconds + 5)
    }

    /// If the view never draws (not on screen), fail instead of hanging.
    private func failIfNoFrame(after limit: Double) {
        DispatchQueue.main.asyncAfter(deadline: .now() + limit) { [self] in
            if !finished {
                print("selftest: no frame was drawn")
                print("selftest: FAIL")
                exit(1)
            }
        }
    }

    /// The same checks driven by the pipeline instead of the clock: a frame
    /// is captured once every tile has decoded `frames`, then `frames + 3`,
    /// and the last at `2 * frames`. `seconds` only limits a stalled run.
    private func startUntilFrames(window: NSWindow, frames: Int) {
        let targets = [frames, frames + 3, 2 * frames]
        func poll(_ stage: Int) {
            guard decodedCounts().allSatisfy({ $0 >= targets[stage] }) else {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.02) { MainActor.assumeIsolated { poll(stage) } }
                return
            }
            grid.view.captureNextFrame { [self] image in
                MainActor.assumeIsolated {
                    if stage == targets.count - 1 { return finish(window: window, image: image) }
                    earlier.append((image, grid.view.renderer.tiles, decodedCounts()))
                    poll(stage + 1)
                }
            }
        }
        poll(0)
        failIfNoFrame(after: seconds)
    }

    private func decodedCounts() -> [Int] { grid.core.tiles.map { $0.decoder.stats.decoded } }

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
        for (i, tile) in grid.core.tiles.enumerated() {
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
            let answerFailures = grid.core.fake.answers(tile: tile.id)?.failures.count ?? 0
            if answerFailures > 0 { problems.append("decode failures: \(answerFailures)") }
            let s = tile.decoder.stats
            print(
                "tile \(i) \(grid.core.names[i]): rect \(Int(rect.minX)),\(Int(rect.minY)) \(Int(rect.width))x\(Int(rect.height)), "
                    + "picture \(size.map { "\(Int($0.width))x\(Int($0.height))" } ?? "none"), "
                    + "decoded \(s.decoded), gated \(grid.core.fake.gated(tile: tile.id)), sessions \(s.sessionsCreated), "
                    + String(format: "spread %.1f ", spread)
                    + (problems.isEmpty ? "OK" : "FAIL"))
            failures += problems.map { "tile \(i): \($0)" }
        }

        failures += grid.core.fake.violations.map { "core contract: \($0)" }
        if failures.isEmpty {
            print("selftest: PASS")
            grid.core.shutdown()
            exit(0)
        }
        for f in failures { print("  \(f)") }
        print("selftest: FAIL")
        grid.core.shutdown()
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
