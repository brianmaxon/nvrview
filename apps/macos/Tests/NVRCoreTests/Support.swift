import CoreGraphics
import FakeCore
import Foundation
import NVRCore
import Synchronization
import VideoKit

enum Fixtures {
    /// `fixtures/synthetic/` at the repository root, located relative to this file.
    static let root: URL = {
        var url = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { url.deleteLastPathComponent() }  // file, NVRCoreTests, Tests, macos, apps
        return url
    }()
    static let directory = root.appendingPathComponent("fixtures/synthetic")

    static func clip(_ name: String, color: StandInColor? = nil) throws -> FakeClip {
        try FakeClip.load(directory.appendingPathComponent(name), colorOverride: color)
    }

    /// The parameter sets (no start codes) of the first access unit.
    static func parameterSets(_ clip: FakeClip) -> [Data] {
        let first = clip.units[0]
        let au = clip.data.subdata(in: first.range)
        return AnnexB.split(au, codec: clip.codec)
            .filter { NALType.isParameterSet($0.type, codec: clip.codec) }
            .map { au.subdata(in: $0.range) }
    }

    static func accessUnit(_ clip: FakeClip, _ index: Int) -> Data {
        clip.data.subdata(in: clip.units[index].range)
    }
}

final class Box<T: Sendable>: Sendable {
    private let value: Mutex<T>
    init(_ initial: T) { value = Mutex(initial) }
    func set(_ v: T) { value.withLock { $0 = v } }
    func get() -> T { value.withLock { $0 } }
    func update(_ body: (inout T) -> Void) { value.withLock { body(&$0) } }
}

/// A `CoreBridge` over a `FakeCore`, one device, events recorded.
final class Rig: Sendable {
    let bridge: CoreBridge
    let fake: FakeCore
    let events = Box<[CoreEvent]>([])
    let device: UInt64

    init(_ options: FakeCore.Options) throws {
        let created = Box<FakeCore?>(nil)
        let events = events
        bridge = try CoreBridge(
            factory: FakeCore.factory(options: options) { created.set($0) },
            onEvent: { event in events.update { $0.append(event) } })
        fake = created.get()!
        device = try bridge.addDevice(host: "192.0.2.1", port: 9000, user: "example-user", password: "example")
    }

    convenience init(_ clip: FakeClip, loop: Bool = false) throws {
        var o = FakeCore.Options(clips: [clip])
        o.loop = loop
        try self.init(o)
    }

    func open(channel: UInt8 = 0) throws -> CoreTile {
        try bridge.openTile(device: device, channel: channel, tier: .main)
    }

    /// Pumps `count` access units, one at a time, waiting for each to decode.
    func play(_ tile: CoreTile, _ count: Int) {
        for _ in 0..<count {
            fake.pump(tile: tile.id, count: 1)
            tile.decoder.waitForPendingFrames()
        }
    }
}

extension RenderedImage {
    func mean(_ r: CGRect) -> SIMD3<Double> {
        var sum = SIMD3<Double>(repeating: 0)
        var n = 0.0
        for y in Int(r.minY)..<Int(r.maxY) {
            for x in Int(r.minX)..<Int(r.maxX) {
                let p = rgb(x: x, y: y)
                sum += SIMD3(Double(p.x), Double(p.y), Double(p.z))
                n += 1
            }
        }
        return sum / max(n, 1)
    }

    func spread(_ r: CGRect) -> Double {
        var values: [Double] = []
        for y in Int(r.minY)..<Int(r.maxY) {
            for x in Int(r.minX)..<Int(r.maxX) {
                let p = rgb(x: x, y: y)
                values.append(Double(p.x + p.y + p.z) / 3)
            }
        }
        let mean = values.reduce(0, +) / Double(max(values.count, 1))
        return (values.map { ($0 - mean) * ($0 - mean) }.reduce(0, +) / Double(max(values.count, 1))).squareRoot()
    }

    var bounds: CGRect { CGRect(x: 0, y: 0, width: width, height: height) }
}

func maxMagnitude<V: SIMD>(_ v: V) -> V.Scalar where V.Scalar: FloatingPoint {
    v.indices.map { Swift.abs(v[$0]) }.max() ?? 0
}
