#if NVRVIEW_REAL_CORE
import Foundation
import NVRCore
import NVRCoreC
import Testing

/// Talk-back through the real core (the XCFramework) and `CCore`, against a
/// spawned mock-nvr whose channel 0 is a doorbell (`--talk 0`): refused
/// without the permission, then started, carried and drained with it, as
/// the app's doorbell call does. Needs `cargo build -p mock-nvr` first.
@Suite(.serialized) struct RealCoreTalkTests {
    private func spawnMock() throws -> (Process, String, UInt16) {
        let mock = Fixtures.root.appendingPathComponent("target/debug/mock-nvr")
        try #require(FileManager.default.isExecutableFile(atPath: mock.path), "build mock-nvr first")
        let p = Process()
        p.executableURL = mock
        p.arguments = [
            "--port", "0", "--channels", "1", "--talk", "0",
            "--fixture", Fixtures.directory.appendingPathComponent("h264-basic.h264").path,
        ]
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

    private func wait(_ timeout: Double, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { Thread.sleep(forTimeInterval: 0.02) }
    }

    @Test func refusedWithoutThePermissionThenCarriedAndDrained() throws {
        let (mock, host, port) = try spawnMock()
        defer { mock.terminate() }
        let events = Box<[CoreEvent]>([])
        let bridge = try CoreBridge(factory: CCore.factory(), onEvent: { e in events.update { $0.append(e) } })
        defer { bridge.shutdown() }
        let device = try bridge.addDevice(host: host, port: port, user: "example-user", password: "example-password")
        let tile = try bridge.openTile(device: device, channel: 0, tier: .sub)
        func talk() -> [TalkEvent] { events.get().compactMap { if case .talk(let t) = $0 { t } else { nil } } }
        wait(10) { events.get().contains { if case .channels = $0 { true } else { false } } }

        // Off by default: refused, nothing sent.
        #expect(throws: CoreStatus.notPermitted) { try bridge.startTalk(tile) }
        wait(2) { !talk().isEmpty }
        #expect(talk().map(\.state) == [.refused])
        #expect(talk().first?.reason == "not_permitted")

        try bridge.setTalkPermission(device: device, channel: 0, allowed: true)
        try bridge.startTalk(tile)
        wait(10) { talk().contains { $0.state == .active || $0.state == .refused && talk().count > 1 } }
        let samples = (0..<16_000).map { Float(0.3 * sin(Double($0) * 2 * Double.pi * 440 / 16_000)) }
        try bridge.pushTalk(tile, samples: samples, sampleRate: 16_000)
        try bridge.stopTalk(tile, drain: true)
        wait(10) { talk().last?.state == .stopped }
        let states = talk()
        print("real core talk: \(states.map { "\($0.state) \($0.reason ?? "") \($0.sentMilliseconds.map { "\($0) ms" } ?? "")" })")
        #expect(states.dropFirst().map(\.state) == [.starting, .active, .stopped])
        #expect(states.last?.reason == "finished")
        // One second pushed: about 1000 ms sent, in 64 ms records.
        #expect(abs((states.last?.sentMilliseconds ?? 0) - 1000) <= 64)
    }
}
#endif
