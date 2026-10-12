#if NVRVIEW_REAL_CORE
import Foundation
import NVRCore
import NVRCoreC
import Testing

/// Capture (minor 9) against the real core and a spawned mock-nvr (needs
/// `cargo build -p mock-nvr` first): a live tile and a call recorded to
/// finalized MP4 files, the call's tracks through a Swift encoder answering
/// from inside its own calls (the C trampolines of CCore), and the limit
/// answer.
@Suite(.serialized) struct RealCoreCaptureTests {
    private func spawnMock() throws -> (Process, String, UInt16) {
        let mock = Fixtures.root.appendingPathComponent("target/debug/mock-nvr")
        try #require(FileManager.default.isExecutableFile(atPath: mock.path), "build mock-nvr first")
        let p = Process()
        p.executableURL = mock
        p.arguments = [
            "--port", "0", "--channels", "1",
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

    private func wait(_ seconds: Double, until done: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if done() { return true }
            Thread.sleep(forTimeInterval: 0.02)
        }
        return done()
    }

    private func recordings(_ events: Box<[CoreEvent]>, _ id: UInt64) -> [RecordingEvent] {
        events.get().compactMap { if case .recording(let r) = $0, r.recording == id { r } else { nil } }
    }

    /// Top-level box types of a file.
    private func boxes(_ path: String) throws -> [String] {
        let d = try Data(contentsOf: URL(fileURLWithPath: path))
        var types: [String] = []
        var at = 0
        while at + 8 <= d.count {
            let size = d[at..<at + 4].reduce(0) { $0 << 8 | Int($1) }
            types.append(String(decoding: d[at + 4..<at + 8], as: UTF8.self))
            guard size >= 8 else { break }
            at += size
        }
        return types
    }

    @Test func liveAndCallRecordings() throws {
        let (mock, host, port) = try spawnMock()
        defer { mock.terminate() }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-capture-\(UUID())")
        defer { try? FileManager.default.removeItem(at: dir) }
        let events = Box<[CoreEvent]>([])
        let bridge = try CoreBridge(factory: CCore.factory(), onEvent: { e in events.update { $0.append(e) } })
        defer { bridge.shutdown() }
        let device = try bridge.addDevice(host: host, port: port, user: "example-user", password: "example-password")
        let tile = try bridge.openTile(device: device, channel: 0, tier: .sub)
        let core = try bridge.capture()
        let live = RecordRequest.live(tile: tile.id, name: "Swift", utcOffsetSeconds: RecordRequest.localUTCOffsetSeconds)

        #expect(throws: RecordStartError(.notConfigured)) { try core.recordStart(live) }
        try core.captureConfigure(json: #"{"live_dir":"\#(dir.path)","min_free_bytes":0,"max_live":1}"#)

        let started = try core.recordStart(live)
        #expect(started.file.hasPrefix(dir.path) && started.file.hasSuffix(" Swift.mp4"))
        #expect(throws: RecordStartError(.limit, limit: "live")) { try core.recordStart(live) }
        #expect(
            wait(20) {
                recordings(events, started.recording).contains { $0.state == "recording" && $0.durationMs >= 2000 }
            })
        try core.recordStop(started.recording)
        #expect(wait(10) { recordings(events, started.recording).contains(where: \.ended) })
        #expect(recordings(events, started.recording).last?.state == "finished")
        let liveBoxes = try boxes(started.file)
        #expect(liveBoxes.first == "ftyp" && liveBoxes.contains("moov") && !liveBoxes.contains("moof"))

        // A call through the Swift encoder: every track is encoded (the
        // mock sends no audio).
        let encoder = AnsweringEncoder()
        encoder.core.withLock { $0 = core }
        try core.captureSetEncoder(encoder)
        let call = try core.recordStart(.call(tile: tile.id, file: dir.appendingPathComponent("call.mp4").path))
        #expect(
            wait(20) {
                recordings(events, call.recording).contains { $0.state == "recording" && $0.durationMs >= 1500 }
            })
        try core.recordStop(call.recording)
        #expect(wait(10) { recordings(events, call.recording).contains(where: \.ended) })
        let end = try #require(recordings(events, call.recording).last)
        #expect(end.state == "finished" && end.tracks == ["video", "Conversation", "Visitor", "You"])
        #expect(encoder.opened.withLock { $0 }.map(\.1) == [0, 1, 2])
        #expect(encoder.closed.withLock { $0 }.map(\.2) == [true, true, true])
        #expect(encoder.refused.load(ordering: .relaxed) == 0)
        let callBoxes = try boxes(call.file)
        #expect(callBoxes.contains("moov") && !callBoxes.contains("moof"))
        try core.captureSetEncoder(nil)
    }
}
#endif
