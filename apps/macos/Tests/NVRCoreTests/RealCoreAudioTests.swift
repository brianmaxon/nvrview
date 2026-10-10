#if NVRVIEW_REAL_CORE
import AVFoundation
import Foundation
import NVRCore
import NVRCoreC
import Testing

/// The real core (the XCFramework from scripts/build-xcframework.sh) end to
/// end: a spawned mock-nvr sends video and AAC clicks (one every 0.5 s) with
/// 20 ms of jitter; the bridge decodes the video; an `AudioPlayer` in
/// offline mode (no speaker) pulls the real render block in real time, as a
/// device would, so `nv_audio_render` sees genuine host times. Checks: the
/// clicks are heard, 500 ms apart, and the core's own A/V error (computed
/// from the host times this bridge passes) stays small. Needs
/// `cargo build -p mock-nvr` first.
@Suite(.serialized) struct RealCoreAudioTests {
    static let root = Fixtures.root

    private func spawnMock() throws -> (Process, String, UInt16) {
        let mock = Self.root.appendingPathComponent("target/debug/mock-nvr")
        try #require(FileManager.default.isExecutableFile(atPath: mock.path), "build mock-nvr first")
        let p = Process()
        p.executableURL = mock
        p.arguments = [
            "--port", "0", "--channels", "1",
            "--fixture", Fixtures.directory.appendingPathComponent("h264-basic.h264").path,
            "--audio", Fixtures.directory.appendingPathComponent("aac-clicks.aac").path,
            "--audio-jitter-ms", "20",
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

    @Test func clicksFromTheMockArePlayedOnTime() throws {
        #expect(CCore.abiMatches)
        let (mock, host, port) = try spawnMock()
        defer { mock.terminate() }
        let stats = Box<[AudioStatsEvent]>([])
        let bridge = try CoreBridge(
            factory: CCore.factory(),
            onEvent: { event in
                if case .audioStats(_, let s) = event { stats.update { $0.append(s) } }
            })
        defer { bridge.shutdown() }
        let device = try bridge.addDevice(host: host, port: port, user: "example-user", password: "example-password")
        let tile = try bridge.openTile(device: device, channel: 0, tier: .main)
        try bridge.setTileAudio(tile, audible: true)

        let rate = 48_000.0
        let chunk: AVAudioFrameCount = 480
        let player = try bridge.makeAudioPlayer(mode: .offline(sampleRate: rate, maximumFrames: 512))
        let lead: UInt64 = 20_000_000
        let base = bridge.clockNowNanos() + lead
        try player.start(hostBaseNanos: base)

        var heard: [Double] = []  // host seconds after `base`
        var quiet = 0
        var frame = 0
        let seconds = 20.0
        while Double(frame) / rate < seconds {
            // Pull each 10 ms buffer `lead` before it is heard, like a device.
            let due = base + UInt64(Double(frame) / rate * 1e9)
            let now = bridge.clockNowNanos()
            if now + lead < due { Thread.sleep(forTimeInterval: Double(due - lead - now) / 1e9) }
            let out = try player.renderOffline(frames: chunk)
            for i in stride(from: 0, to: out.count, by: 2) {
                if abs(out[i]) > 0.4 && quiet > 3000 { heard.append(Double(frame + i / 2) / rate) }
                quiet = abs(out[i]) > 0.4 ? 0 : quiet + 1
            }
            frame += Int(chunk)
        }
        player.stop()

        let last = try #require(stats.get().last)
        // Clicks are 500 ms apart in the fixture, except at its loop seam
        // (63 AAC frames are 4.032 s, so one gap in 8 is longer). While the
        // playout delay shrinks toward its target, audio plays up to 0.2%
        // fast: 499 ms.
        let gaps = zip(heard.dropFirst(), heard).map { $0 - $1 }
        let regular = gaps.filter { $0 < 0.55 }
        let seams = gaps.filter { $0 >= 0.55 }
        let worstGap = regular.map { abs($0 - 0.5) }.max() ?? 1
        let seamSpread = (seams.max() ?? 0) - (seams.min() ?? 0)
        print(
            "real core: \(heard.count) clicks in \(seconds) s, intervals within \(String(format: "%.2f", worstGap * 1e3)) ms of 500 ms "
                + "(\(seams.count) loop seams, spread \(String(format: "%.2f", seamSpread * 1e3)) ms); "
                + "playout delay \(last.playoutDelayMilliseconds ?? -1) ms (target \(last.playoutDelayTargetMilliseconds ?? -1) ms), "
                + "A/V error \(last.avOffsetMilliseconds ?? .nan) ms, underruns \(last.underruns), resyncs \(last.resyncs)")
        #expect(worstGap < 0.0015)
        #expect(seams.count <= heard.count / 7 + 1 && seamSpread < 0.003)
        #expect(heard.count >= 25, "\(heard.count) clicks")
        #expect(last.audible && last.codec == "aac")
        #expect(abs(try #require(last.avOffsetMilliseconds)) < 20)
        #expect(last.resyncs <= 1)
    }
}
#endif
