#if NVRVIEW_REAL_CORE
import Foundation
import NVRCore
import NVRCoreC
import Testing
import VideoKit

/// The recordings screens' calls against the real core and a spawned
/// mock-nvr with a recording store (`--recordings 2`; needs `cargo build -p
/// mock-nvr` first): what the macOS recordings window asks, parsed with the
/// same types it uses, a cover keyframe decoded with VideoToolbox and put
/// back, edits, a lock that downloads, and the Trash hand-over.
@Suite(.serialized) struct RealCoreRecordingsTests {
    private func spawnMock() throws -> (Process, String, UInt16) {
        let mock = Fixtures.root.appendingPathComponent("target/debug/mock-nvr")
        try #require(FileManager.default.isExecutableFile(atPath: mock.path), "build mock-nvr first")
        let p = Process()
        p.executableURL = mock
        p.arguments = [
            "--port", "0", "--channels", "2", "--recordings", "2",
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

    private func object(_ s: String) throws -> [String: Any] {
        try #require(try JSONSerialization.jsonObject(with: Data(s.utf8)) as? [String: Any])
    }

    private func recordings(_ events: Box<[CoreEvent]>) -> [RecordingsEvent] {
        events.get().compactMap { if case .recordings(let r) = $0 { r } else { nil } }
    }

    @Test func theRecordingsWindowsCalls() throws {
        let (mock, host, port) = try spawnMock()
        defer { mock.terminate() }
        let data = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-recordings-\(UUID())")
        defer { try? FileManager.default.removeItem(at: data) }
        let downloads = data.appendingPathComponent("Incidents")
        let events = Box<[CoreEvent]>([])
        let bridge = try CoreBridge(
            factory: CCore.factory(dataDirectory: data.appendingPathComponent("core").path),
            onEvent: { e in events.update { $0.append(e) } })
        defer { bridge.shutdown() }
        try FileManager.default.createDirectory(at: data.appendingPathComponent("core"), withIntermediateDirectories: true)
        let device = try bridge.addDevice(host: host, port: port, user: "example-user", password: "example-password")
        let key = try #require(bridge.deviceKey(device))
        let core = try bridge.recordings()

        // Configure and enable as the app does; the mock's clock is UTC.
        try core.recordingsConfigure(json: #"{"downloads_dir":"\#(downloads.path)"}"#)
        let cams = [RecordingCamera(device: key, channel: 0), RecordingCamera(device: key, channel: 1)]
        try core.recordingsEnable(deviceKey: key, json: RecordingsRequest.enable(cams, utcOffset: 0))
        let today = DeviceTime.today(now: Date(), utcOffset: 0)
        let yesterday = today - DeviceTime.day
        try core.recordingsWant(json: RecordingsRequest.want(cams, days: [yesterday]))
        try core.recordingsHoldToday(1, json: RecordingsRequest.hold(cams))

        // The day arrives by events (one camera-day at a time); then the
        // queries answer from memory.
        let end = yesterday + DeviceTime.day - 1
        #expect(wait(20) {
            let cal = CalendarAnswer.parse(answer: (try? core.recordingsCalendar(
                query: RecordingsRequest.query(start: yesterday, end: end, cameras: cams))) ?? "")
            let ev = EventsAnswer.parse(answer: (try? core.recordingsEvents(
                query: RecordingsRequest.query(start: yesterday, end: end, cameras: cams))) ?? "")
            return cal?.allDays.contains(yesterday) == true && Set(ev?.events.map(\.camera) ?? []) == Set(cams)
        })
        #expect(wait(20) {
            recordings(events).contains { if case .index(key, "idle", 0, _) = $0 { true } else { false } }
        })
        #expect(recordings(events).contains { if case .changed(let d, _, _, _) = $0 { d == key } else { false } })
        let spans = try #require(SpansAnswer.parse(answer: try core.recordingsSpans(
            query: RecordingsRequest.query(start: yesterday, end: end, cameras: cams))))
        #expect(spans.spans.count == 2 && spans.spans.allSatisfy { !$0.spans.isEmpty })
        let status = try #require(IndexStatus.parse(answer: try core.recordingsStatus()))
        #expect(status.devices.map(\.device) == [key])
        let incidents = try #require(IncidentsAnswer.parse(answer: try core.incidentsGet(
            query: RecordingsRequest.query(start: yesterday, end: end)))).incidents
        print("real core: \(spans.spans.map(\.spans.count)) spans, \(incidents.count) incidents")
        #expect(incidents.count >= 2)
        let first = try #require(incidents.first)
        #expect(!first.growing && !first.cameras.isEmpty)
        try core.recordingsHoldToday(1, json: nil)

        // A cover thumbnail: pending, its event, the keyframe decoded here and
        // the small image put back in its place.
        let thumbKey = try #require(first.coverKeys.first)
        var got = try core.thumbnailGet(key: thumbKey.json)
        if got == .pending {
            #expect(wait(20) {
                recordings(events).contains { if case .thumbnail(let k, "ready", _) = $0 { k == thumbKey } else { false } }
            })
            got = try core.thumbnailGet(key: thumbKey.json)
        }
        guard case .ready(let keyframe) = got else {
            Issue.record("no thumbnail: \(got)")
            return
        }
        #expect(keyframe.kind == .h264Keyframe)
        let small = try #require(KeyframeImage.thumbnail(annexB: keyframe.data, codec: .h264))
        #expect(small.width <= 320 && small.height <= 320)
        let image = Thumbnail(
            kind: .image, width: UInt32(small.width), height: UInt32(small.height), ptsMicros: keyframe.ptsMicros,
            data: small.jpeg)
        try core.thumbnailPut(key: thumbKey.json, thumbnail: image)
        #expect(try core.thumbnailGet(key: thumbKey.json) == .ready(image))
        try core.thumbnailCancel(key: nil)

        // Edits: a rename, read back, and reverted.
        let renamed = try #require(EditAnswer.parse(answer: try core.incidentEdit(
            json: IncidentEdit.rename(key: first.key, name: "Delivery").json)))
        #expect(renamed.keys == [first.key])
        let one = try #require(IncidentAnswer.parse(answer: try core.incidentGet(key: first.key))).incident
        #expect(one.name == "Delivery" && one.edited && one.title == "Delivery")
        _ = try core.incidentEdit(json: IncidentEdit.revert(key: first.key).json)
        #expect(try #require(IncidentAnswer.parse(answer: try core.incidentGet(key: first.key))).incident.name == nil)

        // A lock downloads the incident; the download runs to the end.
        let lock = try #require(LockAnswer.parse(answer: try core.incidentLock(key: first.key, locked: true)))
        #expect(lock.downloadStatus == "queued" && !lock.videoNotKept)
        let id = try #require(lock.download)
        var board = DownloadBoard()
        #expect(wait(60) {
            for e in recordings(events) { if case .download(let d) = e { board.apply(d) } }
            return board.byID[id]?.state == "done"
        })
        let done = try #require(board.byID[id])
        print("real core: download \(done.stateText), \(done.outputs.count) files")
        #expect(!done.outputs.isEmpty && done.outputs.allSatisfy { FileManager.default.fileExists(atPath: $0) })
        #expect(DownloadList.parse(try core.downloadsGet())?.contains { $0.download == id } == true)

        // Edit lists over the finished download, and the rest of sound
        // (minor 8): the contract's shapes and status codes.
        let grid = try object(try core.editCompute(json: #"{"kind":"grid","download":"\#(id)","layout":"2up"}"#))
        #expect(grid["kind"] as? String == "grid" && (grid["cells"] as? [Any])?.isEmpty == false)
        let follow = try object(try core.editCompute(json: #"{"kind":"follow","download":"\#(id)"}"#))
        #expect(follow["kind"] as? String == "follow" && (follow["segments"] as? [Any])?.isEmpty == false)
        #expect(throws: CoreStatus.unknownID) { try core.editCompute(json: #"{"kind":"follow","download":"nope"}"#) }
        // Export (minor 10): a trim over the finished files, a range
        // download, and a still plan.
        let trimmed = try object(try core.editCompute(
            json: #"{"kind":"trim","download":"\#(id)","in_ms":0,"out_ms":4000000000000,"share_safe":true}"#))
        #expect((trimmed["outputs"] as? [[String: Any]])?.first?["name"] as? String == "Camera A")
        #expect(throws: CoreStatus.invalidArgument) {
            try core.editCompute(json: #"{"kind":"follow","download":"\#(id)","in_ms":9,"out_ms":3}"#)
        }
        #expect(throws: CoreStatus.invalidArgument) { try core.downloadRange(json: #"{"start":5,"end":4,"cameras":[]}"#) }
        #expect(throws: CoreStatus.unknownID) { try core.stillPlan(json: #"{"playback":987654}"#) }
        #expect(throws: CoreStatus.invalidArgument) { try core.editCompute(json: "{") }
        let cam0 = cams[0].json
        try core.soundWatch(camera: cam0, enabled: true)
        try core.soundWatch(camera: cam0, enabled: false)
        let loud = try object(try core.recordingsLoudness(
            query: #"{"start":\#(yesterday),"end":\#(yesterday + 3599),"bucket_s":60,"cameras":[\#(cam0)]}"#))
        #expect((loud["lanes"] as? [[String: Any]])?.first?["peak_db"] as? [Any] != nil)
        try core.soundLabelsPut(
            json: #"{"camera":\#(cam0),"analyzer":"soundanalysis-v1","start":\#(yesterday),"end":\#(yesterday + 600),"labels":[{"kind":"dog_bark","start":\#(yesterday + 100),"end":\#(yesterday + 105),"confidence":0.9,"label":"dog_bark"}]}"#)
        #expect(throws: CoreStatus.invalidArgument) { try core.soundLabelsPut(json: "{") }
        let analysis = try object(try core.soundAnalyzeDay(json: #"{"camera":\#(cam0),"day":\#(yesterday + 5)}"#))
        #expect(analysis["state"] as? String == "queued" && analysis["request"] as? Int != nil)
        #expect(throws: CoreStatus.unknownID) { try core.soundAnalyzeCancel(987_654_321) }
        #expect(throws: CoreStatus.unknownID) { try core.tileLoudness(987_654_321, capacity: 8) }
        let unlocked = try #require(LockAnswer.parse(answer: try core.incidentLock(key: first.key, locked: false)))
        #expect(unlocked.keys == [first.key] && unlocked.download == nil)

        // The Trash: the core hands the folder over once nothing writes there.
        let trash = try #require(TrashAnswer.parse(answer: try core.downloadTrash(id)))
        board.trashRequested(id, folder: trash.folder)
        var folder: String?
        #expect(wait(10) {
            for e in recordings(events) {
                if case .downloadRemoved(let d, let t, let f) = e, d == id { folder = board.removed(d, trash: t, folder: f) }
            }
            return folder != nil
        })
        #expect(folder == trash.folder && FileManager.default.fileExists(atPath: trash.folder))

        // Camera groups, read back.
        let groups = IncidentGroups(groups: [.init(name: "Front", cameras: cams)])
        try core.cameraGroupsPut(json: groups.putJSON)
        #expect(try #require(IncidentGroups.parse(answer: try core.cameraGroupsGet())).groups == groups.groups)
        #expect(wait(5) { recordings(events).contains { if case .cameraGroupsChanged = $0 { true } else { false } } })
    }
}
#endif
