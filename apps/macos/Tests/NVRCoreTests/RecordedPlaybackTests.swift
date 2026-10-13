import FakeCore
import Foundation
import NVRCore
import NVRViewCore
import Synchronization
import Testing

// Recorded playback (minor 6; docs/FFI.md "Recorded playback (minor 6)"):
// the enums against the header, the request builders, the events of
// docs/FFI-RECORDINGS.md section 6 as the core writes them, and the fake
// core's playback model from open to close.

@Suite struct PlaybackContractTests {
    @Test func enumsMatchTheHeader() {
        #expect(SeekMode.keyframe.rawValue == NV_SEEK_KEYFRAME.rawValue)
        #expect(SeekMode.exact.rawValue == NV_SEEK_EXACT.rawValue)
        #expect(PlaybackState.buffering.rawValue == NV_PLAYBACK_BUFFERING.rawValue)
        #expect(PlaybackState.playing.rawValue == NV_PLAYBACK_PLAYING.rawValue)
        #expect(PlaybackState.paused.rawValue == NV_PLAYBACK_PAUSED.rawValue)
        #expect(PlaybackState.ended.rawValue == NV_PLAYBACK_ENDED.rawValue)
        #expect(PlaybackState.failed.rawValue == NV_PLAYBACK_FAILED.rawValue)
        #expect(NV_ABI_MINOR >= 6)
    }

    @Test func requestsAreTheContractShapes() throws {
        let cam = PlaybackCamera(camera: RecordingCamera(device: "d-3f9a0c1b7e2d4a65", channel: 3), offsetMs: -200)
        #expect(cam.json == #"{"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"tier":"sub","offset_ms":-200}"#)
        let open = PlaybackRequest.range(
            startMs: 1_791_618_120_000, endMs: 1_791_618_146_000, positionMs: 1_791_618_125_000, rate: 0.5,
            cameras: [cam])
        let o = try #require(try JSONSerialization.jsonObject(with: Data(open.utf8)) as? [String: Any])
        #expect(o["start_ms"] as? Int64 == 1_791_618_120_000 && o["position_ms"] as? Int64 == 1_791_618_125_000)
        #expect(o["rate"] as? Double == 0.5 && o["sync"] as? String == "wait")
        #expect((o["cameras"] as? [Any])?.count == 1)
        #expect(PlaybackRequest.incident(key: #"i-"1""#) == #"{"incident":"i-\"1\"","tier":"sub"}"#)
    }
}

@Suite struct PlaybackEventTests {
    private func parse(_ s: String) -> PlaybackEvent? {
        guard case .playback(let e)? = CoreEvent.parse(Data(s.utf8)) else { return nil }
        return e
    }

    @Test func theFourEvents() {
        #expect(
            parse(#"{"type":"playback_state","playback":3,"state":"playing","position_ms":1791618127000,"rate":1.0}"#)
                == .state(playback: 3, state: .playing, positionMs: 1_791_618_127_000, rate: 1))
        #expect(
            parse(#"{"type":"playback_camera","playback":3,"tile":11,"state":"buffering"}"#)
                == .camera(playback: 3, tile: 11, state: "buffering", reason: nil))
        #expect(
            parse(#"{"type":"playback_camera","playback":3,"tile":12,"state":"failed","reason":"start_rejected"}"#)
                == .camera(playback: 3, tile: 12, state: "failed", reason: "start_rejected"))
        // A play the device refused carries its status.
        #expect(
            parse(#"{"type":"playback_camera","playback":3,"tile":12,"state":"failed","reason":"start_rejected","status":400}"#)
                == .camera(playback: 3, tile: 12, state: "failed", reason: "start_rejected", status: 400))
        #expect(
            parse(
                #"{"type":"playback_seek","playback":3,"position_ms":1791618130000,"generation":4,"state":"active","future":1}"#
            ) == .seek(playback: 3, positionMs: 1_791_618_130_000, generation: 4, state: "active"))
        #expect(
            parse(#"{"type":"playback_closed","playback":3,"reason":"core_shutdown"}"#)
                == .closed(playback: 3, reason: "core_shutdown"))
        // Missing what an event cannot do without, or an unknown state: not parsed.
        #expect(parse(#"{"type":"playback_state","playback":3,"state":"warp","position_ms":1}"#) == nil)
        #expect(parse(#"{"type":"playback_seek","playback":3,"state":"active"}"#) == nil)
        #expect(parse(#"{"type":"playback_camera","state":"playing"}"#) == nil)
    }
}

/// A clock a test moves by hand.
private final class TestClock: Sendable {
    let nanos = Atomic<UInt64>(1_000_000_000)
    func advance(ms: UInt64) { nanos.add(ms * 1_000_000, ordering: .relaxed) }
}

/// Answers every Decode and collects the playback events and the tiles that
/// got actions.
private final class PlaybackSink: CoreSink, @unchecked Sendable {
    private let lock = NSLock()
    private var events: [PlaybackEvent] = []
    private var closed: [UInt64: String] = [:]
    private var decodes: [UInt64: Int] = [:]
    var core: (any CoreAPI)?

    func core(didDeliver action: CoreAction, tile: UInt64) {
        core(didDeliver: action, tile: tile, generation: 0)
    }

    func core(didDeliver action: CoreAction, tile: UInt64, generation: UInt32) {
        guard case .decode(let d) = action else { return }
        lock.withLock { decodes[tile, default: 0] += 1 }
        core?.tileDecoded(tile, generation: generation, ptsMicros: d.ptsMicros)
    }

    func core(didEmit json: UnsafeRawBufferPointer) {
        let data = Data(json)
        if case .playback(let e)? = CoreEvent.parse(data) {
            lock.withLock { events.append(e) }
        }
        struct Closed: Decodable {
            var type: String
            var tile: UInt64?
            var reason: String?
        }
        if let c = try? JSONDecoder().decode(Closed.self, from: data), c.type == "tile_closed", let t = c.tile {
            lock.withLock { closed[t] = c.reason }
        }
    }

    func take() -> [PlaybackEvent] {
        lock.withLock {
            defer { events.removeAll() }
            return events
        }
    }

    func closedReason(_ tile: UInt64) -> String? { lock.withLock { closed[tile] } }
    func decodeCount(_ tile: UInt64) -> Int { lock.withLock { decodes[tile] ?? 0 } }
}

@Suite struct FakePlaybackTests {
    @Test func fromOpenToClose() throws {
        let sink = PlaybackSink()
        let clock = TestClock()
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.recordingWorkIsManual = true
        options.playbackClock = { clock.nanos.load(ordering: .relaxed) }
        let fake = FakeCore(options: options, sink: sink)
        let core: any CoreAPI = fake
        sink.core = core
        defer { core.free() }

        let device = try core.addDevice(host: "192.0.2.10", port: 9000, user: "example-user", password: "example-password")
        let key = try core.deviceKey(device)
        let cams = [0, 1].map { PlaybackCamera(camera: RecordingCamera(device: key, channel: $0)) }
        let start: Int64 = 1_791_590_400_000 + 10 * 3_600_000
        let p0 = start + 30_000
        let request = PlaybackRequest.range(startMs: start, endMs: start + 600_000, positionMs: p0, cameras: cams)

        #expect(throws: CoreStatus.notConfigured) { try core.playbackOpen(json: request) }
        try core.recordingsConfigure(json: "{}")
        #expect(throws: CoreStatus.invalidArgument) { try core.playbackOpen(json: request) }
        try core.recordingsEnable(deviceKey: key, json: #"{"channels":"all"}"#)
        #expect(throws: CoreStatus.invalidArgument) {
            try core.playbackOpen(json: PlaybackRequest.range(startMs: start, endMs: start + 1000, rate: 3, cameras: cams))
        }
        #expect(throws: CoreStatus.invalidArgument) {
            try core.playbackOpen(json: PlaybackRequest.range(startMs: start, endMs: start + 1000, cameras: cams + cams))
        }
        #expect(throws: CoreStatus.limit) {
            try core.playbackOpen(
                json: PlaybackRequest.range(startMs: start, endMs: start + 25 * 3_600_000, cameras: cams))
        }
        #expect(throws: CoreStatus.unknownID) { try core.playbackOpen(json: PlaybackRequest.incident(key: "i-none")) }

        // Open: paused at the position; nothing is reported before the call returns.
        let opened = try core.playbackOpen(json: request)
        #expect(opened.tiles.count == 2 && opened.playback != 0 && !opened.tiles.contains(opened.playback))
        #expect(sink.take().isEmpty)
        fake.completePlaybackWork()
        let first = sink.take()
        #expect(first.contains(.seek(playback: opened.playback, positionMs: p0, generation: 1, state: "active")))
        #expect(first.contains(.state(playback: opened.playback, state: .paused, positionMs: p0, rate: 1)))
        #expect(first.contains(.camera(playback: opened.playback, tile: opened.tiles[1], state: "playing", reason: nil)))
        #expect(try core.playbackPosition(opened.playback) == PlaybackPosition(positionMs: p0, state: .paused))

        // The tiles are tiles: actions, answered as for live tiles.
        for t in opened.tiles { _ = fake.pump(tile: t, count: 10) }
        #expect(sink.decodeCount(opened.tiles[0]) > 0 && sink.decodeCount(opened.tiles[1]) > 0)
        #expect(throws: CoreStatus.invalidArgument) { try core.closeTile(opened.tiles[0]) }

        // Play five seconds, then 4x for two.
        try core.playbackPlay(opened.playback)
        clock.advance(ms: 5000)
        #expect(try core.playbackPosition(opened.playback) == PlaybackPosition(positionMs: p0 + 5000, state: .playing))
        #expect(throws: CoreStatus.invalidArgument) { try core.playbackSetRate(opened.playback, rate: 3) }
        try core.playbackSetRate(opened.playback, rate: 4)
        clock.advance(ms: 2000)
        #expect(try core.playbackPosition(opened.playback).positionMs == p0 + 13_000)
        fake.completePlaybackWork()
        #expect(sink.take().contains(.state(playback: opened.playback, state: .playing, positionMs: p0 + 5000, rate: 4)))

        // An exact seek: a new generation.
        try core.playbackSeek(opened.playback, positionMs: start + 120_000, mode: .exact)
        fake.completePlaybackWork()
        #expect(sink.take().contains(.seek(playback: opened.playback, positionMs: start + 120_000, generation: 3, state: "active")))
        try core.playbackSetOffset(opened.playback, tile: opened.tiles[1], offsetMs: 500)
        #expect(throws: CoreStatus.unknownID) { try core.playbackSetOffset(opened.playback, tile: 999, offsetMs: 0) }
        let status = try #require(
            try JSONSerialization.jsonObject(with: Data(try core.playbackStatus(opened.playback).utf8)) as? [String: Any])
        #expect(status["rate"] as? Double == 4)
        #expect((status["cameras"] as? [[String: Any]])?[1]["offset_ms"] as? Int == 500)

        // Pause holds the clock; the end ends it.
        try core.playbackPause(opened.playback)
        clock.advance(ms: 1000)
        #expect(try core.playbackPosition(opened.playback).positionMs == start + 120_000)
        try core.playbackPlay(opened.playback)
        clock.advance(ms: 600_000)
        #expect(try core.playbackPosition(opened.playback) == PlaybackPosition(positionMs: start + 600_000, state: .ended))

        // Remove and add a camera.
        try core.playbackRemoveCamera(opened.playback, tile: opened.tiles[1])
        #expect(sink.closedReason(opened.tiles[1]) == "closed")
        #expect(throws: CoreStatus.unknownID) { try core.playbackRemoveCamera(opened.playback, tile: opened.tiles[1]) }
        let added = try core.playbackAddCamera(opened.playback, json: cams[1].json)
        #expect(!opened.tiles.contains(added))
        #expect(throws: CoreStatus.invalidArgument) { try core.playbackAddCamera(opened.playback, json: cams[1].json) }

        // Close: every tile closes, then playback_closed; the playback is gone.
        _ = sink.take()
        try core.playbackClose(opened.playback)
        #expect(sink.closedReason(opened.tiles[0]) == "closed" && sink.closedReason(added) == "closed")
        #expect(sink.take().last == .closed(playback: opened.playback, reason: "closed"))
        #expect(throws: CoreStatus.unknownID) { try core.playbackPosition(opened.playback) }
        #expect(throws: CoreStatus.unknownID) { try core.playbackClose(opened.playback) }
        #expect(fake.violations.isEmpty, "\(fake.violations)")
    }

    @Test func anIncidentOpensItsCamerasAndRange() throws {
        let sink = PlaybackSink()
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.recordingWorkIsManual = true
        let fake = FakeCore(options: options, sink: sink)
        let core: any CoreAPI = fake
        sink.core = core
        defer { core.free() }
        let device = "d-3f9a0c1b7e2d4a65"
        let day: Int64 = 1_791_590_400
        try core.recordingsConfigure(json: "{}")
        try core.recordingsEnable(deviceKey: device, json: #"{"channels":[{"channel":0}]}"#)
        try core.recordingsWant(json: #"{"cameras":[{"device":"\#(device)","channel":0}],"days":[\#(day)]}"#)
        let list = try #require(
            try JSONSerialization.jsonObject(
                with: Data(try core.incidentsGet(query: #"{"start":\#(day),"end":\#(day + 86_399)}"#).utf8))
                as? [String: Any])
        let incident = try #require((list["incidents"] as? [[String: Any]])?.first?["key"] as? String)
        let opened = try core.playbackOpen(json: PlaybackRequest.incident(key: incident))
        #expect(opened.tiles.count == 1)
        // The device is not added: the camera fails, the playback still opens.
        fake.completePlaybackWork()
        #expect(
            sink.take().contains(
                .camera(playback: opened.playback, tile: opened.tiles[0], state: "failed", reason: "session_closed")))
        let status = try #require(
            try JSONSerialization.jsonObject(with: Data(try core.playbackStatus(opened.playback).utf8)) as? [String: Any])
        let startMs = try #require(status["start_ms"] as? Int64)
        let endMs = try #require(status["end_ms"] as? Int64)
        #expect(endMs > startMs && startMs % 1000 == 0)
        try core.playbackClose(opened.playback)
    }
}
