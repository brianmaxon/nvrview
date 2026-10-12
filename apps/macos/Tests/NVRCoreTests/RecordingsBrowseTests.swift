import FakeCore
import Foundation
import NVRCore
import Testing

/// The recordings screens' pure pieces (RecordingsBrowse.swift).
@Suite struct RecordingsBrowseTests {
    let oct1: Int64 = 1_790_812_800  // 2026-10-01 00:00:00, a Thursday
    let oct10: Int64 = 1_791_590_400

    @Test func deviceDays() {
        #expect(DeviceTime.dayStart(oct10 + 3600) == oct10)
        #expect(DeviceTime.dayStart(oct10) == oct10)
        #expect(DeviceTime.dayStart(-1) == -86_400)
        // 23:30 UTC is already the next day two hours east of UTC.
        let late = Date(timeIntervalSince1970: TimeInterval(oct10 + 23 * 3600 + 1800))
        #expect(DeviceTime.today(now: late, utcOffset: 0) == oct10)
        #expect(DeviceTime.today(now: late, utcOffset: 7200) == oct10 + 86_400)
        #expect(DeviceTime.time(oct10 + 7 * 3600 + 42 * 60 + 5) == "07:42:05")
        #expect(DeviceTime.span(oct10 + 5, oct10 + 65) == "00:00:05–00:01:05")
        #expect(DeviceTime.dayOfMonth(oct10) == 10)
    }

    @Test func months() {
        #expect(DeviceTime.month(containing: oct10) == (oct1, oct1 + 31 * 86_400))
        #expect(DeviceTime.month(oct10, adding: -1) == oct1 - 30 * 86_400)
        #expect(DeviceTime.month(oct10, adding: 3) == 1_798_761_600)  // 2027-01-01
        let sundayFirst = DeviceTime.weeks(ofMonth: oct10, firstWeekday: 1)
        #expect(sundayFirst.count == 5 && sundayFirst.allSatisfy { $0.count == 7 })
        #expect(sundayFirst[0].prefix(4).allSatisfy { $0 == nil } && sundayFirst[0][4] == oct1)
        #expect(sundayFirst.joined().compactMap { $0 }.count == 31)
        let mondayFirst = DeviceTime.weeks(ofMonth: oct10, firstWeekday: 2)
        #expect(mondayFirst[0][3] == oct1)
    }

    @Test func kindFilters() {
        for animal in ["dog_cat", "dog", "cat", "bird", "animal", "wild_animal"] {
            #expect(KindFilter(kind: animal) == .animal, "\(animal)")
            #expect(IndicatorKind(detectionKind: animal) == .animal)
        }
        #expect(KindFilter(kind: "dog_bark") == .other)
        #expect(KindFilter(kind: "people") == .people)
        #expect(KindFilter(kind: "vehicle") == .vehicle)
        #expect(KindFilter(kind: "motion") == .motion)
        #expect(KindFilter(kind: "package") == .other)
        #expect(KindFilter.passes(["people", "dog_cat"], [.animal]))
        #expect(!KindFilter.passes(["people"], [.animal, .vehicle]))
        #expect(KindFilter.passes(["anything"], []))
    }

    @Test func theContractsAnswersParse() throws {
        let incidents = try #require(IncidentsAnswer.parse(answer: """
            {"revision": 12, "incidents": [
             {"key": "i-1791618125-d-3f9a0c1b7e2d4a65-3-0-5d2e8a41", "id": "1791618125-9c1e5a2b7d40f316",
              "name": null, "group": "Group 1", "start": 1791618125, "end": 1791618160,
              "margin_s": 5, "kinds": ["vehicle"], "growing": false, "edited": false, "locked": false,
              "cameras": [
               {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "name": "Camera 1",
                "intervals": [[1791618125, 1791618141]], "download": [1791618120, 1791618146], "added": false}],
              "events": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
                          "kind": "vehicle", "source": "device", "start": 1791618125, "end": 1791618141}],
              "downloads": ["1791618125-9c1e5a2b7d40f316"], "future": 1}]}
            """))
        let i = try #require(incidents.incidents.first)
        #expect(i.group == "Group 1" && i.name == nil && i.kinds == ["vehicle"])
        #expect(i.title == "07:42:05 Vehicle")
        #expect(i.coverKeys == [
            ThumbnailKey(camera: RecordingCamera(device: "d-3f9a0c1b7e2d4a65", channel: 3), start: 1_791_618_125, end: 1_791_618_141)
        ])
        let spans = try #require(SpansAnswer.parse(answer: """
            {"revision": 41, "spans": [{"camera": {"device": "d-1", "channel": 3, "lens": 0},
              "spans": [{"start": 1791590400, "end": 1791594000, "bytes": 1073741824, "causes": ["sched", "md"], "tier": "main"}]}]}
            """))
        #expect(spans.spans.first?.spans.first?.causes == ["sched", "md"])
        let cal = try #require(CalendarAnswer.parse(answer: #"{"revision":41,"days":[{"camera":{"device":"d-1","channel":3},"days":[1,2]},{"camera":{"device":"d-1","channel":4},"days":[2,3]}]}"#))
        #expect(cal.allDays == [1, 2, 3])
        let lock = try #require(LockAnswer.parse(answer: #"{"revision":3,"keys":["i-a"],"download":null,"folder":null,"download_status":"not_configured"}"#))
        #expect(lock.videoNotKept && lock.message?.contains("not being kept") == true)
        let status = try #require(IndexStatus.parse(answer: #"{"revision":4,"devices":[{"device":"d-1","state":"busy","pending_days":3,"revision":4}]}"#))
        #expect(status.devices.first?.pendingDays == 3)
        let groups = try #require(IncidentGroups.parse(answer: """
            {"revision": 3, "groups": [{"name": "Group 1", "cameras": [
              {"device": "d-1", "channel": 3}, {"device": "d-1", "channel": 5, "lens": 1}]}],
             "camera_names": [{"camera": {"device": "d-1", "channel": 3}, "name": "Camera 1"}]}
            """))
        #expect(groups.groups.first?.cameras.last == RecordingCamera(device: "d-1", channel: 5, lens: 1))
        #expect(groups.cameraNames.first?.name == "Camera 1")
    }

    @Test func requestsAndEdits() {
        let c = RecordingCamera(device: "d-1", channel: 3)
        #expect(RecordingsRequest.want([c], days: [5, 6], force: true)
            == #"{"cameras":[{"device":"d-1","channel":3,"lens":0}],"days":[5,6],"force":true}"#)
        #expect(RecordingsRequest.enable([c, RecordingCamera(device: "d-1", channel: 5, lens: 1)], utcOffset: -3600)
            == #"{"channels":[{"channel":3,"lens":0},{"channel":5,"lens":1}],"utc_offset_s":-3600}"#)
        #expect(RecordingsRequest.query(start: 1, end: 2, group: "Yard \"east\"")
            == #"{"start":1,"end":2,"group":"Yard \"east\""}"#)
        #expect(RecordingsRequest.hold([c]) == #"{"cameras":[{"device":"d-1","channel":3,"lens":0}]}"#)
        #expect(IncidentEdit.merge(["a", "b"]).json == #"{"op":"merge","keys":["a","b"]}"#)
        #expect(IncidentEdit.split(key: "a", at: 9).json == #"{"op":"split","key":"a","at":9}"#)
        #expect(IncidentEdit.rename(key: "a", name: "Delivery").json == #"{"op":"rename","key":"a","name":"Delivery"}"#)
        #expect(IncidentEdit.rename(key: "a", name: "  ").json == #"{"op":"rename","key":"a","name":null}"#)
        #expect(IncidentEdit.addCamera(key: "a", camera: c).json
            == #"{"op":"add_camera","key":"a","camera":{"device":"d-1","channel":3,"lens":0}}"#)
        #expect(IncidentEdit.revert(key: "a").json == #"{"op":"revert","key":"a"}"#)
    }

    @Test func groupsPutDropsEmptyGroupsAndRepeats() throws {
        let a = RecordingCamera(device: "d-1", channel: 0), b = RecordingCamera(device: "d-1", channel: 1)
        let groups = IncidentGroups(groups: [.init(name: "Front", cameras: [a, b]), .init(name: " ", cameras: [a]),
                                             .init(name: "Back", cameras: [b])])
        let back = try #require(IncidentGroups.parse(answer: groups.putJSON))
        #expect(back.groups == [.init(name: "Front", cameras: [a, b])])
        #expect(!groups.putJSON.contains("revision"))
    }

    private func progress(_ id: String, _ state: String, chunks: Int = 0, paused: Bool = false) -> DownloadProgress {
        DownloadProgress.parse(answer: """
            {"download":"\(id)","folder":"/x/\(id)","state":"\(state)","paused":\(paused),"cameras":[
             {"camera":{"device":"d-1","channel":0},"state":"\(state)","chunks_done":\(chunks),"chunks_total":4,"bytes":0}]}
            """)!
    }

    @Test func downloadsBoard() {
        var board = DownloadBoard()
        var changed = board.apply(progress("a", "queued"))
        #expect(changed)
        changed = board.apply(progress("a", "running", chunks: 1))
        #expect(changed)
        changed = board.apply(progress("a", "running", chunks: 2))
        #expect(!changed, "progress alone is not a state change")
        #expect(board.downloads.first?.fraction == 0.5)
        changed = board.apply(progress("b", "queued"))
        #expect(changed)
        #expect(board.downloads.map(\.download) == ["a", "b"])
        // Forget: removed, no Trash.
        var folder = board.removed("b", trash: false, folder: nil)
        #expect(folder == nil)
        // Trash: gone from the list at once; a late event does not bring it
        // back; the folder goes to the Trash on download_removed.
        board.trashRequested("a", folder: "/x/a")
        #expect(board.downloads.isEmpty)
        changed = board.apply(progress("a", "running", chunks: 3))
        #expect(!changed)
        #expect(board.downloads.isEmpty)
        folder = board.removed("a", trash: true, folder: "/x/a")
        #expect(folder == "/x/a")
        #expect(board.trashing.isEmpty)
        #expect(progress("c", "failed").stateText == "Failed")
        #expect(progress("c", "done").fraction == 1 && !progress("c", "done").isWorking)
        #expect(progress("c", "running", paused: true).isWorking == false)
    }

    @Test func thumbnailsBoardIsBounded() {
        var board = ThumbnailBoard(capacity: 3)
        let keys = (0..<5).map { ThumbnailKey(camera: RecordingCamera(device: "d-1", channel: 0), start: Int64($0), end: Int64($0) + 1) }
        #expect(board.needsRequest(keys[0]))
        board.set(keys[0], .pending)
        #expect(!board.needsRequest(keys[0]))
        board.set(keys[1], .image(Data([1])))
        board.set(keys[2], .image(Data([2])))
        board.touch(keys[1])
        board.set(keys[3], .empty)
        // The pending one stays; the least recently used finished one goes.
        #expect(board.count == 3 && board[keys[0]] == .pending && board[keys[2]] == nil && board[keys[1]] != nil)
        board.set(keys[4], .dropped)
        #expect(board.needsRequest(keys[4]))
        board.forgetPending()
        #expect(board[keys[0]] == nil && board.pendingKeys.isEmpty)
    }
}

/// The fake core's incidents and edits, which the recordings screens are
/// built against before the real core.
@Suite struct FakeIncidentEditTests {
    private func core() throws -> (any CoreAPI, String, Int64) {
        var options = FakeCore.Options(clips: [])
        options.recordingWorkIsManual = true
        let core: any CoreAPI = FakeCore(options: options, sink: QuietSink())
        let device = "d-1"
        let day: Int64 = 1_791_590_400
        try core.recordingsConfigure(json: #"{"downloads_dir":"/downloads"}"#)
        try core.recordingsEnable(deviceKey: device, json: #"{"channels":[{"channel":0},{"channel":1},{"channel":2}]}"#)
        try core.recordingsWant(json: RecordingsRequest.want((0..<3).map { RecordingCamera(device: device, channel: $0) }, days: [day]))
        return (core, device, day)
    }

    private func incidents(_ core: any CoreAPI, _ day: Int64) throws -> [Incident] {
        try #require(IncidentsAnswer.parse(answer: try core.incidentsGet(query: RecordingsRequest.query(start: day, end: day + 86_399)))).incidents
    }

    @Test func mergeSplitCamerasRevert() throws {
        let (core, device, day) = try core()
        defer { core.free() }
        var all = try incidents(core, day)
        #expect(all.count == 7)
        #expect(all.filter { $0.kinds == ["dog_cat"] }.count == 1)
        let people = all.filter { $0.kinds == ["people"] }.sorted { $0.start < $1.start }
        let merged = try #require(EditAnswer.parse(answer: try core.incidentEdit(json: IncidentEdit.merge([people[0].key, people[1].key]).json)))
        #expect(merged.keys == [people[0].key])
        all = try incidents(core, day)
        #expect(all.count == 6 && all.first { $0.key == people[0].key }?.cameras.count == 2)
        #expect(throws: CoreStatus.invalidArgument) {
            try core.incidentEdit(json: IncidentEdit.split(key: people[0].key, at: people[0].start).json)
        }
        let split = try #require(EditAnswer.parse(answer: try core.incidentEdit(json: IncidentEdit.split(key: people[0].key, at: people[1].start).json)))
        #expect(Set(split.keys) == [people[0].key, people[1].key])
        let ch2 = RecordingCamera(device: device, channel: 2)
        _ = try core.incidentEdit(json: IncidentEdit.addCamera(key: people[0].key, camera: ch2).json)
        let added = try #require(try incidents(core, day).first { $0.key == people[0].key })
        #expect(added.cameras.last?.added == true && added.edited)
        #expect(throws: CoreStatus.invalidArgument) {
            try core.incidentEdit(json: IncidentEdit.removeCamera(key: people[2].key, camera: people[2].cameras[0].camera).json)
        }
        _ = try core.incidentEdit(json: IncidentEdit.revert(key: people[0].key).json)
        #expect(try incidents(core, day).allSatisfy { !$0.edited } && (try incidents(core, day)).count == 7)
    }

    @Test func aGroupJoinsItsCameras() throws {
        let (core, device, day) = try core()
        defer { core.free() }
        let front = IncidentGroups(groups: [.init(name: "Front", cameras: [RecordingCamera(device: device, channel: 0), RecordingCamera(device: device, channel: 1)])])
        try core.cameraGroupsPut(json: front.putJSON)
        let all = try incidents(core, day)
        #expect(all.count == 5)
        let grouped = try #require(IncidentsAnswer.parse(answer: try core.incidentsGet(query: RecordingsRequest.query(start: day, end: day + 86_399, group: "Front")))).incidents
        #expect(grouped.count == 2 && grouped.allSatisfy { $0.cameras.count == 2 && $0.group == "Front" })
    }
}

/// A sink that drops everything.
private final class QuietSink: CoreSink, @unchecked Sendable {
    func core(didDeliver action: CoreAction, tile: UInt64) {}
    func core(didEmit json: UnsafeRawBufferPointer) {}
}
