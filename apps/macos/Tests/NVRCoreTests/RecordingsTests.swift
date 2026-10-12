import FakeCore
import Foundation
import NVRCore
import NVRViewCore
import Testing

// Recordings, part 1 (minor 5; docs/FFI.md "Recordings (minor 5)"): the
// status codes and thumbnail values against the header, the events of
// docs/FFI-RECORDINGS.md section 8 as the core writes them, and the fake
// core's made-up recording store from configure to the Trash.

@Suite struct RecordingsContractTests {
    @Test func statusCodesAndThumbnailKindsMatchTheHeader() {
        #expect(CoreStatus.unsupported.code == NV_ERR_UNSUPPORTED.rawValue)
        #expect(CoreStatus.notConfigured.code == NV_ERR_NOT_CONFIGURED.rawValue)
        #expect(CoreStatus.pending.code == NV_PENDING.rawValue)
        #expect(CoreStatus(NV_PENDING) == .pending)
        #expect(ThumbnailKind.h264Keyframe.rawValue == NV_THUMB_H264_KEYFRAME.rawValue)
        #expect(ThumbnailKind.h265Keyframe.rawValue == NV_THUMB_H265_KEYFRAME.rawValue)
        #expect(ThumbnailKind.image.rawValue == NV_THUMB_IMAGE.rawValue)
    }

    @Test func thumbnailsConvertToAndFromTheCStruct() {
        let t = Thumbnail(kind: .image, width: 64, height: 36, ptsMicros: 1234, data: Data([1, 2, 3]))
        let info = t.info
        #expect(info.kind == NV_THUMB_IMAGE && info.width == 64 && info.height == 36 && info.pts_us == 1234)
        #expect(Thumbnail(info, data: t.data) == t)
        #expect(Thumbnail(NvThumbnail(kind: NvThumbKind(rawValue: 9), width: 0, height: 0, pts_us: 0), data: Data()) == nil)
        #expect(MemoryLayout<NvThumbnail>.size == 24)
    }

    @Test func requestObjectsAreTheContractShapes() throws {
        let cam = RecordingCamera(device: "d-3f9a0c1b7e2d4a65", channel: 3)
        #expect(cam.json == #"{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0}"#)
        let key = ThumbnailKey(camera: cam, start: 1_791_618_125, end: 1_791_618_141)
        let back = try JSONDecoder().decode(ThumbnailKey.self, from: Data(key.json.utf8))
        #expect(back == key && back.tier == "sub")
        #expect(
            try JSONDecoder().decode(RecordingCamera.self, from: Data(#"{"device":"d-1","channel":2}"#.utf8))
                == RecordingCamera(device: "d-1", channel: 2, lens: 0))
    }
}

@Suite struct RecordingsEventTests {
    private func parse(_ s: String) -> RecordingsEvent? {
        guard case .recordings(let e)? = CoreEvent.parse(Data(s.utf8)) else { return nil }
        return e
    }

    private let cam = RecordingCamera(device: "d-3f9a0c1b7e2d4a65", channel: 3)

    @Test func soundEvents() {
        #expect(
            parse(
                #"{"type":"sound_watch","camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"state":"refused","reason":"no_audio"}"#
            ) == .soundWatch(camera: cam, state: "refused", reason: "no_audio"))
        #expect(
            parse(
                #"{"type":"sound_analyze","request":4,"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"day":1791590400,"state":"done","seconds":86000,"labels":3}"#
            ) == .soundAnalyze(
                request: 4, camera: cam, day: 1_791_590_400, state: "done", reason: nil, seconds: 86_000, labels: 3))
        // Missing what they cannot do without.
        #expect(parse(#"{"type":"sound_analyze","state":"done"}"#) == nil)
        #expect(parse(#"{"type":"sound_watch","state":"watching"}"#) == nil)
        #expect(TileLoudness.decibels(0) == nil && TileLoudness.decibels(1) == -127 && TileLoudness.decibels(128) == 0)
    }

    @Test func indexEvents() {
        #expect(
            parse(
                #"{"type":"recordings_index","device":"d-3f9a0c1b7e2d4a65","state":"busy","pending_days":11,"revision":40}"#)
                == .index(device: "d-3f9a0c1b7e2d4a65", state: "busy", pendingDays: 11, revision: 40))
        #expect(
            parse(
                #"{"type":"recordings_day_failed","device":"d-3f9a0c1b7e2d4a65","camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"day":1791504000,"reason":"session closed during the search"}"#
            )
                == .dayFailed(
                    device: "d-3f9a0c1b7e2d4a65", camera: cam, day: 1_791_504_000,
                    reason: "session closed during the search"))
        #expect(
            parse(
                #"{"type":"recordings_changed","device":"d-3f9a0c1b7e2d4a65","revision":41,"start":1791590400,"end":1791676799,"future":true}"#
            ) == .changed(device: "d-3f9a0c1b7e2d4a65", revision: 41, start: 1_791_590_400, end: 1_791_676_799))
        #expect(parse(#"{"type":"incidents_changed","revision":12}"#) == .incidentsChanged(revision: 12))
        #expect(parse(#"{"type":"camera_groups_changed","revision":3}"#) == .cameraGroupsChanged(revision: 3))
        // Missing what an event cannot do without: not parsed.
        #expect(parse(#"{"type":"recordings_changed","device":"d-1"}"#) == nil)
        #expect(parse(#"{"type":"incidents_changed"}"#) == nil)
    }

    @Test func thumbnailEvents() {
        let e = parse(
            #"{"type":"thumbnail","key":{"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"tier":"sub","start":1791618125,"end":1791618141},"state":"ready"}"#
        )
        #expect(
            e == .thumbnail(key: ThumbnailKey(camera: cam, start: 1_791_618_125, end: 1_791_618_141), state: "ready", reason: nil))
        let failed = parse(
            #"{"type":"thumbnail","key":{"camera":{"device":"d-1","channel":0},"tier":"main","start":1,"end":2},"state":"failed","reason":"the device is not enabled and added"}"#
        )
        guard case .thumbnail(let key, "failed", let reason)? = failed else {
            Issue.record("\(String(describing: failed))")
            return
        }
        #expect(key.tier == "main" && key.camera.lens == 0 && reason != nil)
    }

    @Test func downloadEvents() throws {
        let e = parse(
            #"{"type":"download","download":"1791618125-9c1e5a2b7d40f316","incident":"i-x","folder":"/downloads/2026-10-10 07.42 vehicle — Camera 1","state":"running","paused":false,"cameras":[{"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":3,"lens":0},"name":"Camera 1","state":"running","paused":false,"chunks_done":1,"chunks_total":2,"bytes":20971520,"bytes_estimate":41943040,"output":null},{"camera":{"device":"d-3f9a0c1b7e2d4a65","channel":5,"lens":0},"name":"Camera 2","state":"failed","paused":false,"chunks_done":0,"chunks_total":2,"bytes":0,"reason":"stalled","detail":"no data for 10 s (attempt 8 of 8)","output":null}]}"#
        )
        guard case .download(let d)? = e else {
            Issue.record("\(String(describing: e))")
            return
        }
        #expect(d.download == "1791618125-9c1e5a2b7d40f316" && d.state == "running" && !d.paused)
        #expect(d.folder?.hasSuffix("Camera 1") == true)
        #expect(d.cameras.count == 2)
        #expect(d.cameras[0].chunksDone == 1 && d.cameras[0].bytesEstimate == 41_943_040 && d.cameras[0].output == nil)
        #expect(d.cameras[1].state == "failed" && d.cameras[1].reason == "stalled")
        // The short form of section 8.
        guard case .download(let short)? = parse(#"{"type":"download","download":"a","state":"done","cameras":[]}"#) else {
            Issue.record("short download event")
            return
        }
        #expect(short.cameras.isEmpty && short.incident == nil)
        #expect(
            parse(#"{"type":"download_removed","download":"a","trash":false}"#)
                == .downloadRemoved(download: "a", trash: false, folder: nil))
        #expect(
            parse(#"{"type":"download_removed","download":"a","trash":true,"folder":"/d/x"}"#)
                == .downloadRemoved(download: "a", trash: true, folder: "/d/x"))
    }

    @Test func deviceInfoStillParsesAndCarriesTheFlags() {
        let json = #"{"type":"device_info","device":1,"key":"d-1","name":"NVR","model":"EXAMPLE-MODEL","recordings":{"search":true,"playback":true,"download":true,"thumbnails":true}}"#
        #expect(CoreEvent.parse(Data(json.utf8)) == .deviceInfo(device: 1, name: "NVR", model: "EXAMPLE-MODEL"))
        #expect(
            RecordingSupport.from(deviceInfo: Data(json.utf8))
                == RecordingSupport(search: true, playback: true, download: true, thumbnails: true))
        #expect(RecordingSupport.from(deviceInfo: Data(#"{"type":"device_info","device":1}"#.utf8)) == RecordingSupport())
    }
}

/// Collects the parsed events a core sends.
private final class EventLog: CoreSink, @unchecked Sendable {
    private let lock = NSLock()
    private var events: [RecordingsEvent] = []

    func core(didDeliver action: CoreAction, tile: UInt64) {}
    func core(didEmit json: UnsafeRawBufferPointer) {
        guard case .recordings(let e)? = CoreEvent.parse(Data(json)) else { return }
        lock.withLock { events.append(e) }
    }

    func take() -> [RecordingsEvent] {
        lock.withLock {
            defer { events.removeAll() }
            return events
        }
    }
}

@Suite struct FakeRecordingsTests {
    private func object(_ s: String) throws -> [String: Any] {
        try #require(try JSONSerialization.jsonObject(with: Data(s.utf8)) as? [String: Any])
    }

    /// Analyzing a day from the synthetic downloads, as the core does from
    /// finished ones: `no_source` without a downloads folder, `no_audio`
    /// without a finished download, then heard seconds, a label, and a cancel.
    @Test func analyzingADayFromDownloads() throws {
        let device = "d-3f9a0c1b7e2d4a65"
        let day: Int64 = 1_791_590_400
        let cam = RecordingCamera(device: device, channel: 0)

        func make(downloads: Bool) throws -> (any CoreAPI, FakeCore, EventLog) {
            let log = EventLog()
            var options = FakeCore.Options(clips: [])
            options.recordingWorkIsManual = true
            let core: any CoreAPI = FakeCore(options: options, sink: log)
            let fake = try #require(core as? FakeCore)
            try core.recordingsConfigure(json: downloads ? #"{"downloads_dir":"/downloads"}"# : "{}")
            try core.recordingsEnable(deviceKey: device, json: #"{"channels":[{"channel":0},{"channel":1}]}"#)
            try core.recordingsWant(
                json: #"{"cameras":[\#(cam.json)],"days":[\#(day + 100)],"force":false}"#)
            _ = log.take()
            return (core, fake, log)
        }
        func analyze(_ core: any CoreAPI, _ fake: FakeCore, _ log: EventLog) throws -> [SoundAnalysis.Entry] {
            _ = log.take()
            _ = try core.soundAnalyzeDay(json: SoundAnalysis.query(camera: cam, day: day))
            fake.completeRecordingWork()
            var entry: SoundAnalysis.Entry?
            var all: [SoundAnalysis.Entry] = []
            for e in log.take() {
                guard case .soundAnalyze(let r, _, _, let state, let reason, let seconds, let labels) = e,
                    let s = SoundAnalysis.State(rawValue: state)
                else { continue }
                entry = SoundAnalysis.fold(entry, request: r, state: s, reason: reason, seconds: seconds, labels: labels)
                all.append(entry!)
            }
            return all
        }

        let (noSource, noSourceFake, noSourceLog) = try make(downloads: false)
        #expect(try analyze(noSource, noSourceFake, noSourceLog).map(\.state) == [.queued, .failed])
        #expect(try analyze(noSource, noSourceFake, noSourceLog).last?.reason == "no_source")

        let (core, fake, log) = try make(downloads: true)
        let none = try analyze(core, fake, log)
        #expect(none.map(\.state) == [.queued, .failed] && none.last?.reason == "no_audio")
        let loud = #"{"start":\#(day),"end":\#(day + 86_399),"bucket_s":3600,"cameras":[\#(cam.json)]}"#
        func heardBuckets() throws -> Int {
            let lanes = try #require(try object(try core.recordingsLoudness(query: loud))["lanes"] as? [[String: Any]])
            return (lanes.first?["peak_db"] as? [Any])?.filter { !($0 is NSNull) }.count ?? -1
        }
        #expect(try heardBuckets() == 0)

        let incidents = try #require(
            try object(try core.incidentsGet(query: #"{"start":\#(day),"end":\#(day + 86_399)}"#))["incidents"] as? [[String: Any]])
        let key = try #require(incidents.first { ($0["cameras"] as? [[String: Any]])?.contains { ($0["camera"] as? [String: Any])?["channel"] as? Int == 0 } == true }?["key"] as? String)
        _ = try core.downloadIncident(json: #"{"incident":"\#(key)"}"#)
        // Queued, not finished: still nothing to hear.
        _ = try core.soundAnalyzeDay(json: SoundAnalysis.query(camera: cam, day: day))
        try core.soundAnalyzeCancel(try #require(log.take().compactMap { e -> UInt64? in
            if case .soundAnalyze(let r, _, _, "queued", _, _, _) = e { r } else { nil }
        }.first))
        #expect(try heardBuckets() == 0)
        fake.completeRecordingWork()
        _ = log.take()
        let done = try analyze(core, fake, log)
        #expect(done.map(\.state) == [.queued, .running, .done])
        #expect((done.last?.seconds ?? 0) > 0 && done.last?.labels == 1)
        #expect(try heardBuckets() > 0)
        #expect(SoundAnalysis.outcome(done.last!)?.hasPrefix("Analyzed ") == true)
    }

    @Test func fromConfigureToTheTrash() throws {
        let log = EventLog()
        var options = FakeCore.Options(clips: [])
        options.recordingWorkIsManual = true
        let core: any CoreAPI = FakeCore(options: options, sink: log)
        let fake = try #require(core as? FakeCore)
        let device = "d-3f9a0c1b7e2d4a65"
        let day: Int64 = 1_791_590_400

        #expect(throws: CoreStatus.notConfigured) { try core.recordingsEnable(deviceKey: device, json: "{}") }
        #expect(throws: CoreStatus.invalidArgument) { try core.recordingsConfigure(json: "{") }
        try core.recordingsConfigure(json: #"{"downloads_dir":"/downloads"}"#)
        try core.recordingsEnable(deviceKey: device, json: #"{"channels":[{"channel":0},{"channel":1}]}"#)
        try core.soundConfigure(json: #"{"alarms":true}"#)

        let cams = #"[{"device":"\#(device)","channel":0},{"device":"\#(device)","channel":1}]"#
        #expect(throws: CoreStatus.invalidArgument) {
            try core.recordingsWant(json: #"{"cameras":[{"device":"\#(device)","channel":7}],"days":[\#(day)]}"#)
        }
        let many = (0..<32).map { String(day - Int64($0) * 86_400) }.joined(separator: ",")
        #expect(throws: CoreStatus.limit) { try core.recordingsWant(json: #"{"cameras":\#(cams),"days":[\#(many)]}"#) }
        try core.recordingsWant(json: #"{"cameras":\#(cams),"days":[\#(day + 100)],"force":false}"#)
        let indexEvents = log.take()
        #expect(indexEvents.contains(.changed(device: device, revision: 1, start: day, end: day + 86_399)))
        #expect(indexEvents.contains(.index(device: device, state: "idle", pendingDays: 0, revision: 1)))
        #expect(indexEvents.contains { if case .incidentsChanged = $0 { true } else { false } })

        let query = #"{"start":\#(day),"end":\#(day + 86_399)}"#
        let events = try object(try core.recordingsEvents(query: query))["events"] as? [[String: Any]]
        #expect(events?.count == 4)
        #expect(try object(try core.recordingsCalendar(query: query))["days"] as? [Any] != nil)
        #expect(try object(try core.recordingsSpans(query: query))["truncated"] as? Bool == false)
        #expect((try object(try core.recordingsStatus())["devices"] as? [Any])?.count == 1)

        try core.recordingsHoldToday(7, json: #"{"cameras":\#(cams)}"#)
        #expect(fake.recordingsHolds[7]?.count == 2)
        #expect(throws: CoreStatus.invalidArgument) { try core.recordingsHoldToday(0, json: nil) }
        try core.recordingsHoldToday(7, json: nil)
        #expect(fake.recordingsHolds[7] == nil)

        // Incidents, newest first; one by key; a rename.
        let incidents = try #require(try object(try core.incidentsGet(query: query))["incidents"] as? [[String: Any]])
        #expect(incidents.count == 4)
        let incident = try #require(incidents.last?["key"] as? String)
        #expect(throws: CoreStatus.unknownID) { try core.incidentGet(key: "i-unknown") }
        let edited = try object(try core.incidentEdit(json: #"{"op":"rename","key":"\#(incident)","name":"Delivery"}"#))
        #expect(edited["keys"] as? [String] == [incident])
        let one = try object(try core.incidentGet(key: incident))["incident"] as? [String: Any]
        #expect(one?["name"] as? String == "Delivery" && one?["edited"] as? Bool == true)

        // Groups.
        try core.cameraGroupsPut(json: #"{"groups":[{"name":"Group 1","cameras":\#(cams)}]}"#)
        #expect((try object(try core.cameraGroupsGet())["groups"] as? [Any])?.count == 1)
        #expect(log.take().contains(.cameraGroupsChanged(revision: 1)))
        // A group changes the keys of its incidents (FFI-RECORDINGS.md
        // section 3); without groups the key above names the incident again.
        #expect(throws: CoreStatus.unknownID) { try core.incidentGet(key: incident) }
        try core.cameraGroupsPut(json: #"{"groups":[]}"#)
        _ = log.take()

        // A thumbnail: pending, then ready after its event, then the app's image.
        let thumbKey = ThumbnailKey(camera: RecordingCamera(device: device, channel: 0), start: day + 28_800, end: day + 28_815)
        #expect(try core.thumbnailGet(key: thumbKey.json) == .pending)
        fake.completeRecordingWork()
        #expect(log.take().contains(.thumbnail(key: thumbKey, state: "ready", reason: nil)))
        guard case .ready(let t) = try core.thumbnailGet(key: thumbKey.json) else {
            Issue.record("the thumbnail is not ready")
            return
        }
        #expect(t.kind == .h264Keyframe && !t.data.isEmpty)
        let image = Thumbnail(kind: .image, width: 64, height: 36, ptsMicros: 0, data: Data([0xff, 0xd8, 0xff, 0xd9]))
        try core.thumbnailPut(key: thumbKey.json, thumbnail: image)
        #expect(try core.thumbnailGet(key: thumbKey.json) == .ready(image))
        try core.thumbnailCancel(key: nil)

        // A download to the end, a lock that finds it, and the Trash.
        let queued = try object(try core.downloadIncident(json: #"{"incident":"\#(incident)"}"#))
        let id = try #require(queued["download"] as? String)
        #expect(queued["existing"] as? Bool == false)
        #expect((try object(try core.downloadIncident(json: #"{"incident":"\#(incident)"}"#)))["existing"] as? Bool == true)
        fake.completeRecordingWork()
        let progress = log.take().compactMap { e -> DownloadProgress? in
            if case .download(let d) = e { d } else { nil }
        }
        #expect(progress.map(\.state) == ["queued", "running", "done"])
        #expect(progress.last?.cameras.first?.output != nil)
        #expect(DownloadList.parse(try core.downloadsGet())?.map(\.download) == [id])
        // Edit lists over the finished download, and sound (minor 8).
        #expect(throws: CoreStatus.unknownID) { try core.editCompute(json: #"{"kind":"follow","download":"nope"}"#) }
        #expect(throws: CoreStatus.unsupported) { try core.editCompute(json: #"{"kind":"trim","download":"\#(id)"}"#) }
        #expect(throws: CoreStatus.unsupported) {
            try core.editCompute(json: #"{"kind":"grid","download":"\#(id)","in_ms":1}"#)
        }
        #expect(throws: CoreStatus.invalidArgument) { try core.editCompute(json: #"{"kind":"nonsense","download":"\#(id)"}"#) }
        let follow = try object(try core.editCompute(json: #"{"kind":"follow","download":"\#(id)"}"#))
        #expect(follow["kind"] as? String == "follow" && (follow["segments"] as? [[String: Any]])?.count == 1)
        let grid = try object(try core.editCompute(json: #"{"kind":"grid","download":"\#(id)","layout":"4up"}"#))
        #expect(grid["layout"] as? String == "4up" && (grid["cells"] as? [Any])?.count == 1)

        let cam1 = #"{"device":"\#(device)","channel":1,"lens":0}"#
        try core.soundWatch(camera: cam1, enabled: true)
        try core.soundWatch(camera: cam1, enabled: true)
        try core.soundWatch(camera: cam1, enabled: false)
        let sound = log.take().compactMap { e -> String? in
            if case .soundWatch(_, let state, _) = e { state } else { nil }
        }
        #expect(sound == ["watching", "stopped"])
        let lanes = try object(try core.recordingsLoudness(query: #"{"start":\#(day),"end":\#(day + 119),"bucket_s":60}"#))
        #expect((lanes["lanes"] as? [Any])?.isEmpty == true)
        #expect(throws: CoreStatus.limit) {
            try core.recordingsLoudness(query: #"{"start":0,"end":200000,"bucket_s":1}"#)
        }
        try core.soundLabelsPut(
            json: #"{"camera":\#(cam1),"analyzer":"soundanalysis-v1","start":\#(day),"end":\#(day + 600),"labels":[{"kind":"dog_bark","start":\#(day + 100),"end":\#(day + 105)}]}"#)
        #expect(throws: CoreStatus.invalidArgument) {
            try core.soundLabelsPut(
                json: #"{"camera":\#(cam1),"analyzer":"pattern-v1","start":\#(day),"end":\#(day + 600),"labels":[]}"#)
        }
        let analysis = try object(try core.soundAnalyzeDay(json: #"{"camera":\#(cam1),"day":\#(day)}"#))
        let request = try #require(analysis["request"] as? Int)
        fake.completeRecordingWork()
        let states = log.take().compactMap { e -> String? in
            if case .soundAnalyze(let r, _, _, let state, _, _, _) = e, r == UInt64(request) { state } else { nil }
        }
        // Analyzed from the finished download when it holds that camera.
        let downloadedCam1 = progress.last?.cameras.contains { $0.camera == RecordingCamera(device: device, channel: 1) } == true
        #expect(states == (downloadedCam1 ? ["queued", "running", "done"] : ["queued", "failed"]))
        #expect(throws: CoreStatus.unknownID) { try core.soundAnalyzeCancel(UInt64(request)) }
        #expect(throws: CoreStatus.unknownID) { try core.tileLoudness(987_654_321, capacity: 8) }

        let locked = try object(try core.incidentLock(key: incident, locked: true))
        #expect(locked["download_status"] as? String == "existing" && locked["download"] as? String == id)
        #expect(throws: CoreStatus.unknownID) { try core.downloadPause("no-such-download", camera: nil, paused: true) }
        let trashed = try object(try core.downloadTrash(id))
        let folder = try #require(trashed["folder"] as? String)
        #expect(log.take().contains(.downloadRemoved(download: id, trash: true, folder: folder)))
        #expect(throws: CoreStatus.unknownID) { try core.downloadForget(id, deletePartial: false) }
        let unlocked = try object(try core.incidentLock(key: incident, locked: false))
        #expect(unlocked["keys"] as? [String] == [incident] && unlocked["download"] == nil)

        core.free()
        #expect(throws: CoreStatus.closed) { try core.recordingsStatus() }
        #expect(fake.violations.isEmpty, "\(fake.violations)")
    }

    @Test func aCoreWithoutRecordingsRefusesEverything() throws {
        // The protocol's defaults, as for a core older than minor 5.
        struct Old: CoreAPI {
            func addDevice(host: String, port: UInt16, user: String, password: String) throws(CoreStatus) -> UInt64 { 0 }
            func removeDevice(_ device: UInt64) throws(CoreStatus) {}
            func openTile(device: UInt64, channel: UInt8, tier: StreamTier) throws(CoreStatus) -> UInt64 { 0 }
            func closeTile(_ tile: UInt64) throws(CoreStatus) {}
            func tileDecoded(_ tile: UInt64, ptsMicros: Int64) -> CoreStatus { .ok }
            func tileDecodeFailed(_ tile: UInt64, kind: CoreDecodeFailure) -> CoreStatus { .ok }
            func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool)
                throws(CoreStatus) -> [TileLayout]
            { [] }
            func audioSetOutput(sampleRate: UInt32, channels: UInt32) throws(CoreStatus) {}
            func tileAudioSet(_ tile: UInt64, audible: Bool, volume: Float) throws(CoreStatus) {}
            func tileAudioTap(_ tile: UInt64, enabled: Bool) throws(CoreStatus) {}
            var audioRenderHook: AudioRenderHook { .silent }
            func clockNowNanos() -> UInt64 { 0 }
            func free() {}
        }
        let old: any CoreAPI = Old()
        #expect(throws: CoreStatus.unsupported) { try old.recordingsConfigure(json: "{}") }
        #expect(throws: CoreStatus.unsupported) { try old.thumbnailGet(key: "{}") }
        #expect(throws: CoreStatus.unsupported) { try old.downloadsGet() }
    }
}
