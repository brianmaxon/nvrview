#if NVRVIEW_REAL_CORE
import Foundation
import NVRCore
import NVRCoreC
import Testing

/// ABI 3 against the real core and a spawned mock-nvr (needs
/// `cargo build -p mock-nvr` first): a focus-style tier switch through the
/// bridge with two decoders, device keys, the layouts store with the app's
/// JSON, and detection events.
@Suite(.serialized) struct RealCoreABI3Tests {
    private func spawnMock(_ extra: [String] = []) throws -> (Process, String, UInt16) {
        let mock = Fixtures.root.appendingPathComponent("target/debug/mock-nvr")
        try #require(FileManager.default.isExecutableFile(atPath: mock.path), "build mock-nvr first")
        let p = Process()
        p.executableURL = mock
        p.arguments =
            ["--port", "0", "--channels", "2", "--fixture", Fixtures.directory.appendingPathComponent("h264-basic.h264").path]
            + extra
        p.environment = ["MOCK_NVR_PASSWORD": "example-password"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        try p.run()
        var text = ""
        let deadline = Date().addingTimeInterval(5)
        while !text.contains("\n"), Date() < deadline {
            text += String(decoding: out.fileHandleForReading.availableData, as: UTF8.self)
        }
        let line = try #require(text.split(separator: "\n").first { $0.hasPrefix("mock-nvr listening on ") })
        let addr = line.dropFirst("mock-nvr listening on ".count)
        let colon = try #require(addr.lastIndex(of: ":"))
        return (p, String(addr[..<colon]), try #require(UInt16(addr[addr.index(after: colon)...])))
    }

    private func wait(_ seconds: Double, until done: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if done() { return true }
            Thread.sleep(forTimeInterval: 0.02)
        }
        return done()
    }

    @Test func tierSwitchKeysAndLayouts() throws {
        let (mock, host, port) = try spawnMock()
        defer { mock.terminate() }
        let data = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-core-\(UUID())")
        defer { try? FileManager.default.removeItem(at: data) }
        let events = Box<[CoreEvent]>([])
        let bridge = try CoreBridge(
            factory: CCore.factory(dataDirectory: data.path), onEvent: { e in events.update { $0.append(e) } })
        defer { bridge.shutdown() }
        let device = try bridge.addDevice(host: host, port: port, user: "example-user", password: "example-password")

        // Device keys: d- and 16 hex digits, the same for the same address.
        let key = try #require(bridge.deviceKey(device))
        #expect(key.hasPrefix("d-") && key.count == 18)

        let tile = try bridge.openTile(device: device, channel: 0, tier: .sub)
        #expect(wait(10) { tile.latestPicture() != nil })
        try bridge.setTileTier(tile, .main)
        #expect(wait(20) { tile.tierState.shown == .main })
        #expect(tile.tierState.shownGeneration >= 1)
        #expect(wait(2) { tile.decoders.count == 1 })
        #expect(tile.latestPicture() != nil)
        let tiers = events.get().compactMap { e -> TierSwitchState? in
            if case .tileTier(let t, _, let s, _, _) = e, t == tile.id { return s }
            return nil
        }
        #expect(tiers.first == .starting)
        #expect(tiers.contains(.active))
        // The tier already requested: nothing starts, reported active again.
        try bridge.setTileTier(tile, .main)
        try bridge.setTileTier(tile, .sub)
        let back = wait(20) { tile.tierState.shown == .sub }
        if !back {
            print("tile state \(tile.tierState), decoders \(tile.decoders.count)")
            for e in events.get() { if case .tileTier = e { print("  \(e)") } }
        }
        #expect(back)
        print("real core tier switch: \(tiers), timings \(events.get().compactMap { e -> String? in if case .timing(let w, _, let ns) = e { return "\(w) \(ns / 1_000_000) ms" }; return nil })")

        // Layouts: the app's JSON round-trips; the core versions it.
        let layout = SavedLayout(
            id: "l1", name: "Layout 1",
            root: .split(.row, ratio: 0.62, a: .leaf(CameraRef(device: key, channel: 0)), b: .leaf(nil)))
        try bridge.putLayout(json: layout.json)
        var stored = layout
        stored.version = 1
        #expect(SavedLayout.list(fromJSON: try bridge.layoutsJSON()) == [stored])
        #expect(wait(2) { events.get().contains { if case .layoutsChanged(let r) = $0 { r != nil } else { false } } })
        try bridge.deleteLayout(id: "l1")
        #expect(SavedLayout.list(fromJSON: try bridge.layoutsJSON()).isEmpty)
    }

    @Test func detectionsFromTheMock() throws {
        let (mock, host, port) = try spawnMock(["--aitype", "7", "--detections", "1"])
        defer { mock.terminate() }
        let events = Box<[CoreEvent]>([])
        let bridge = try CoreBridge(factory: CCore.factory(), onEvent: { e in events.update { $0.append(e) } })
        defer { bridge.shutdown() }
        let device = try bridge.addDevice(host: host, port: port, user: "example-user", password: "example-password")
        let got = wait(10) {
            events.get().contains { if case .detection(let d) = $0 { d.device == device && d.active } else { false } }
        }
        #expect(got)
        let kinds = Set(events.get().compactMap { e -> String? in if case .detection(let d) = e { return d.kind }; return nil })
        print("real core detections: \(kinds.sorted())")
    }
}
#endif
