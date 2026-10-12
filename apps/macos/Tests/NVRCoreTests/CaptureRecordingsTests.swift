import Foundation
import NVRCore
import Testing

/// The recordings' pure side (docs/CAPTURE.md): the settings mapping, the
/// pills, the messages for each limit and each end, the board of running
/// recordings, the header's pill priority, and the call recording's shared
/// stem.
@Suite struct CaptureRecordingsTests {
    private func object(_ json: String) throws -> [String: Any] {
        try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }

    static func event(_ id: UInt64, _ state: String, reason: String?, ms: UInt64, kind: String) -> RecordingEvent {
        let r = reason.map { ",\"reason\":\"\($0)\"" } ?? ""
        let json = #"""
            {"recording":\#(id),"kind":"\#(kind)","device":1,"channel":2,"lens":0,"tier":"main","state":"\#(state)"\#(r),
             "file":"/x/a.mp4","segment":1,"bytes":0,"duration_ms":\#(ms),"gaps":0,"audio":"none","tracks":["video"],
             "encode_dropped_ms":0}
            """#
        return RecordingEvent.parse(Data(json.utf8))!
    }

    @Test func settingsMapToTheConfigureJSON() throws {
        var s = CaptureSettings(liveFolder: "/x/Movies/NVRView")
        var o = try object(s.json)
        #expect(o["live_dir"] as? String == "/x/Movies/NVRView")
        #expect(o["quality"] as? String == "main" && o["share_safe"] as? Bool == false)
        #expect(o["max_duration_s"] as? Int == 3600 && o["max_bytes"] as? Int == 4_000_000_000)
        #expect(o["min_free_bytes"] as? Int == 2_000_000_000 && o["stop_free_bytes"] as? Int == 1_000_000_000)
        #expect(o["max_live"] as? Int == 4 && o["max_calls"] as? Int == 2)

        s.quality = .sub
        s.shareSafe = true
        s.maxMinutes = 0
        s.maxBytes = 1
        o = try object(s.json)
        #expect(o["quality"] as? String == "sub" && o["share_safe"] as? Bool == true)
        // Clamped to 1 minute and 100 MB.
        #expect(o["max_duration_s"] as? Int == 60 && o["max_bytes"] as? Int == 100_000_000)
        s.maxMinutes = 100_000
        s.maxBytes = .max
        o = try object(s.json)
        #expect(o["max_duration_s"] as? Int == 12 * 3600 && o["max_bytes"] as? Int == 64_000_000_000)
    }

    @Test func pills() {
        #expect(RecordingText.tilePill(ms: 83_400) == "● REC 1:23")
        #expect(RecordingText.callPill(ms: 42_000) == "● Recording 0:42")
        #expect(RecordingText.elapsed(ms: 3_723_000) == "1:02:03")
        #expect(RecordingText.elapsed(ms: 0) == "0:00")
    }

    @Test func limitMessagesNameTheLimit() {
        let s = CaptureSettings(liveFolder: "/x")
        func text(_ limit: String) -> (String, Bool) {
            RecordingText.refused(RecordStartError(.limit, limit: limit), settings: s, call: false)
        }
        #expect(text("live").0.contains("4 cameras") && !text("live").1)
        #expect(text("calls").0.contains("2 calls"))
        #expect(text("main_streams").0.contains("two main streams") && text("main_streams").1)
        #expect(text("disk_low").0.contains("free disk space") && text("disk_low").0.contains("2 GB"))
        #expect(RecordingText.refused(RecordStartError(.notConfigured), settings: s, call: true).text.contains("encoder"))
        #expect(RecordingText.refused(RecordStartError(.unknownID), settings: s, call: false).text.contains("not open"))
    }

    @Test func endMessages() {
        var s = CaptureSettings(liveFolder: "/x")
        s.maxMinutes = 120
        func event(_ state: String, _ reason: String?, kind: String = "live") -> RecordingEvent {
            Self.event(1, state, reason: reason, ms: 0, kind: kind)
        }
        #expect(RecordingText.ended(event("finished", "stopped"), settings: s) == "Recording saved")
        #expect(RecordingText.ended(event("finished", "stopped", kind: "call"), settings: s) == "Call recording saved")
        #expect(RecordingText.ended(event("finished", "max_duration"), settings: s) == "Recording stopped at its 2 hours limit")
        #expect(RecordingText.ended(event("finished", "max_size"), settings: s) == "Recording stopped at its 4 GB limit")
        #expect(RecordingText.ended(event("finished", "disk_low"), settings: s)?.contains("almost full") == true)
        #expect(RecordingText.ended(event("finished", "core_shutdown"), settings: s) == nil)
        #expect(RecordingText.ended(event("recovered", nil), settings: s)?.contains("unfinished") == true)
    }

    @Test func theBoardFollowsStartsAndEvents() {
        let cam = CameraRef(device: "d1", channel: 2)
        var b = RecordingBoard()
        b.begin(7, kind: "live", camera: cam, file: "/x/a.mp4", tier: "main")
        #expect(b.pill(for: cam) == "● REC 0:00" && b.live(of: cam)?.id == 7 && b.call(of: cam) == nil)
        func event(_ id: UInt64, _ state: String, ms: UInt64, kind: String = "live") -> RecordingEvent {
            Self.event(id, state, reason: state == "finished" ? "stopped" : nil, ms: ms, kind: kind)
        }
        #expect(b.apply(event(7, "recording", ms: 83_000)) == nil)
        #expect(b.pill(for: cam) == "● REC 1:23")
        // A call of another camera; the events of an ID the app never
        // started are ignored.
        let other = CameraRef(device: "d9", channel: 2)
        b.begin(8, kind: "call", camera: other, file: "/x/b.mp4", tier: "sub")
        b.apply(event(8, "recording", ms: 1000, kind: "call"))
        b.apply(event(9, "recording", ms: 1000))
        #expect(b.call(of: other)?.id == 8 && b.list.map(\.id) == [7, 8])
        b.rekey("d1", to: "k1")
        #expect(b.live(of: CameraRef(device: "k1", channel: 2))?.id == 7)
        let end = b.apply(event(7, "finished", ms: 90_000))
        #expect(end?.recording == 7 && b.ended.last?.recording == 7)
        #expect(b.pill(for: CameraRef(device: "k1", channel: 2)) == nil)
        // An unknown recording's end is ignored.
        #expect(b.apply(event(99, "finished", ms: 0)) == nil)
    }

    @Test func thePillNeverOverflows() {
        // Every width down to the narrowest keeps the recording pill.
        let tile = TileHeaderTile(
            audio: true, talk: true, doorbell: true, panTilt: true, presets: true, lens: true, fisheye: true,
            picture: true, recording: true, zoomed: true, panned: false, alert: false, listening: false,
            conversation: false, call: .noCall)
        for width in stride(from: 60.0, through: 600, by: 10) {
            let layout = TileHeaderLayout.compute(width: width, titleWidth: 200, tile: tile, previous: nil)
            #expect(layout.visible.first == .recordingPill, "width \(width)")
            #expect(!layout.overflow.contains(.recordingPill))
        }
        // Record becomes Stop while recording; the per-stream items go.
        let menu = TileHeaderLayout.menu(for: tile)
        #expect(menu.first { $0.item == .record }?.label == "Stop Recording")
        #expect(!menu.contains { $0.item == .recordMain || $0.item == .recordSub })
    }

    @Test func callRecordingAndTranscriptShareAStem() {
        let name = "2026-10-11 17.02.03 Camera 1.txt"
        #expect(CallTranscriptFile.sharedStem(name, taken: []) == "2026-10-11 17.02.03 Camera 1")
        #expect(
            CallTranscriptFile.sharedStem(name, taken: ["2026-10-11 17.02.03 camera 1.MP4"])
                == "2026-10-11 17.02.03 Camera 1 2")
        #expect(
            CallTranscriptFile.sharedStem(
                name, taken: ["2026-10-11 17.02.03 Camera 1.txt", "2026-10-11 17.02.03 Camera 1 2.partial.mp4"])
                == "2026-10-11 17.02.03 Camera 1 3")
    }
}
