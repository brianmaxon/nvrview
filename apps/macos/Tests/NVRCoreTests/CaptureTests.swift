import FakeCore
import Foundation
import NVRCore
import NVRViewCore
import Synchronization
import Testing

// Capture (minor 9; docs/FFI.md "Capture"): the request builder, the
// `recording` event as the core writes it, and the fake core's capture
// model from configure to finished, with an encoder that answers from
// inside its own calls.

/// An encoder that answers every frame (and two more on a flush) through
/// the core, as the platform encoders will.
final class AnsweringEncoder: CaptureEncoder {
    let core = Mutex<(any CoreAPI)?>(nil)
    let opened = Mutex<[(UInt64, UInt32)]>([])
    let closed = Mutex<[(UInt64, UInt32, Bool)]>([])
    let refused = Atomic<Int>(0)

    func open(recording: UInt64, track: UInt32, sampleRate: UInt32, channels: UInt32, bitRate: UInt32)
        -> EncoderOpened?
    {
        opened.withLock { $0.append((recording, track)) }
        return EncoderOpened(audioSpecificConfig: [0x11, 0x88], primingFrames: 2112)
    }

    func encode(recording: UInt64, track: UInt32, pcm: UnsafeBufferPointer<Float>) {
        if pcm.count != 1024 { refused.add(1, ordering: .relaxed) }
        answer(recording, track)
    }

    func close(recording: UInt64, track: UInt32, flush: Bool) {
        if flush {
            answer(recording, track)
            answer(recording, track)
        }
        closed.withLock { $0.append((recording, track, flush)) }
    }

    private func answer(_ recording: UInt64, _ track: UInt32) {
        let unit: [UInt8] = [0, 0, 0, 7]
        let status = unit.withUnsafeBytes { core.withLock { $0 }?.recordAudioEncoded(recording: recording, track: track, unit: $0) }
        if status != .ok { refused.add(1, ordering: .relaxed) }
    }
}

@Suite struct CaptureContractTests {
    @Test func requestsAreTheContractShapes() {
        #expect(
            RecordRequest.live(tile: 7, quality: "sub", name: "Camera 1", utcOffsetSeconds: 3600).json
                == #"{"kind":"live","name":"Camera 1","quality":"sub","tile":7,"utc_offset_s":3600}"#)
        #expect(
            RecordRequest.call(tile: 7, file: "/tmp/a b.mp4").json
                == #"{"file":"/tmp/a b.mp4","kind":"call","tile":7}"#)
        #expect(NV_ABI_MINOR >= 9)
        #expect(NV_ASC_MAX == 64)
    }

    @Test func theRecordingEvent() throws {
        let json = #"""
            {"type":"recording","recording":12,"kind":"live","device":1,"channel":3,"lens":0,"tier":"main",
             "state":"finished","reason":"max_duration","file":"/x/a.mp4","segment":1,"bytes":1048576000,
             "duration_ms":3600000,"gaps":1,"audio":"passthrough","tracks":["video","audio"],
             "encode_dropped_ms":0,"dropped_frames":2,"fragmented":false,"future":true}
            """#
        guard case .recording(let e)? = CoreEvent.parse(Data(json.utf8)) else {
            Issue.record("not a recording event")
            return
        }
        #expect(e.recording == 12 && e.kind == "live" && e.channel == 3 && e.tier == "main")
        #expect(e.state == "finished" && e.reason == "max_duration" && e.ended)
        #expect(e.durationMs == 3_600_000 && e.bytes == 1_048_576_000 && e.gaps == 1)
        #expect(e.tracks == ["video", "audio"] && e.droppedFrames == 2 && e.fragmented == false)
    }

    @Test func limitAnswersNameTheLimit() {
        #expect(RecordStartError(.limit, answer: #"{"limit":"disk_low"}"#) == RecordStartError(.limit, limit: "disk_low"))
        #expect(RecordStartError(.notConfigured, answer: "").limit == nil)
    }
}

@Suite struct FakeCaptureTests {
    private func recordings(_ rig: Rig, _ id: UInt64) -> [RecordingEvent] {
        rig.events.get().compactMap { if case .recording(let r) = $0, r.recording == id { r } else { nil } }
    }

    @Test func liveAndCallRecordings() throws {
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.recordingWorkIsManual = true
        let rig = try Rig(options)
        defer { rig.bridge.shutdown() }
        let tile = try rig.open()
        let core = try rig.bridge.capture()
        let live = RecordRequest.live(tile: tile.id, name: "Camera 1", utcOffsetSeconds: 0)

        // Before the configuration.
        #expect(throws: RecordStartError(.notConfigured)) { try core.recordStart(live) }
        #expect(throws: CoreStatus.invalidArgument) { try core.captureConfigure(json: #"{"live_dir":"rel"}"#) }
        try core.captureConfigure(json: #"{"live_dir":"/tmp/nvrview-captures","max_live":1}"#)

        // Low disk at the start.
        rig.fake.setCaptureFreeBytes(1)
        #expect(throws: RecordStartError(.limit, limit: "disk_low")) { try core.recordStart(live) }
        rig.fake.setCaptureFreeBytes(.max)
        #expect(throws: RecordStartError(.unknownID)) {
            try core.recordStart(.live(tile: tile.id + 99, utcOffsetSeconds: 0))
        }

        let started = try core.recordStart(live)
        #expect(started.state == "waiting_keyframe")
        #expect(started.file.hasPrefix("/tmp/nvrview-captures/") && started.file.hasSuffix(" Camera 1.mp4"))
        #expect(throws: RecordStartError(.limit, limit: "live")) { try core.recordStart(live) }
        // Nothing arrives before the call that caused it returned.
        #expect(recordings(rig, started.recording).isEmpty)
        rig.fake.completeCaptureWork()
        #expect(recordings(rig, started.recording).map(\.state) == ["waiting_keyframe", "recording"])
        #expect(try core.recordStatus().contains(#""recording":\#(started.recording)"#))
        try core.recordStop(started.recording)
        #expect(throws: CoreStatus.unknownID) { try core.recordStop(started.recording) }
        rig.fake.completeCaptureWork()
        let end = try #require(recordings(rig, started.recording).last)
        #expect(end.state == "finished" && end.reason == "stopped" && end.ended)
        #expect(try core.recordStatus() == #"{"recordings":[]}"#)

        // A call needs an encoder; with one, its tracks go through it.
        let call = RecordRequest.call(tile: tile.id, file: "/tmp/nvrview-captures/call.mp4")
        #expect(throws: RecordStartError(.notConfigured)) { try core.recordStart(call) }
        let encoder = AnsweringEncoder()
        encoder.core.withLock { $0 = core }
        try core.captureSetEncoder(encoder)
        let c = try core.recordStart(call)
        #expect(c.file == "/tmp/nvrview-captures/call.mp4")
        #expect(throws: RecordStartError(.limit, limit: "calls")) { try core.recordStart(call) }
        let tracks = encoder.opened.withLock { $0 }.filter { $0.0 == c.recording }.map(\.1)
        #expect(tracks == [0, 2] || tracks == [0, 1, 2])
        #expect(rig.fake.encodedUnits(recording: c.recording).values.allSatisfy { $0 == 1 })
        try core.recordStop(c.recording)
        #expect(encoder.closed.withLock { $0 }.allSatisfy { $0.0 == c.recording && $0.2 })
        #expect(encoder.refused.load(ordering: .relaxed) == 0)
        rig.fake.completeCaptureWork()
        let callEnd = try #require(recordings(rig, c.recording).last)
        #expect(callEnd.kind == "call" && callEnd.tracks == ["video", "Conversation", "Visitor", "You"])
        // The recording ended: a late unit is refused.
        let late = [UInt8](repeating: 1, count: 4).withUnsafeBytes {
            core.recordAudioEncoded(recording: c.recording, track: 0, unit: $0)
        }
        #expect(late == .unknownID)
        try core.captureSetEncoder(nil)
    }
}
