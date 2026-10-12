import Foundation
import NVRCore

// Recordings, part 1 (minor 5) in the fake core: a made-up recording store
// with the contract's JSON (docs/FFI-RECORDINGS.md, sections 1 to 5 and 8),
// for the recordings views (milestone P, M1) before they meet the real core.
// No device and no files: every enabled camera recorded every day all day,
// with a person at 08:00 and a vehicle at 17:00 (device-local seconds).
// Asking for days indexes them at once (`recordings_index` busy, then
// `recordings_changed`, `recordings_index` idle, `incidents_changed`).
// Thumbnail fetches and downloads are queued and finish shortly after the
// call (or only on `completeRecordingWork()` with
// `Options.recordingWorkIsManual`): a fetch makes a stand-in keyframe, a
// download goes to `done` with an output path that is never written. Edits
// apply `rename` and `revert`; the other operations answer `.unsupported`.

/// What the fake core keeps for recordings, under one lock.
final class FakeRecordingsState: @unchecked Sendable {
    struct Download {
        var id: String
        var incident: String
        var folder: String
        var cameras: [RecordingCamera]
        var state: String
        var paused: Bool
    }

    struct World {
        var configured = false
        var downloadsDir: String?
        /// Allowed cameras per device key (nil: every channel, first lens).
        var enabled: [String: [RecordingCamera]?] = [:]
        var indexed: [String: Set<Int64>] = [:]
        var revision = 0
        var incidentsRevision = 0
        var names: [String: String] = [:]
        var locks: Set<String> = []
        var groups: Any = [String: Any]()
        var groupsRevision = 0
        var holds: [UInt64: [RecordingCamera]] = [:]
        var thumbnails: [ThumbnailKey: Thumbnail] = [:]
        var pendingThumbnails: [ThumbnailKey] = []
        var downloads: [Download] = []
        var nextDownload = 1
        var stopped = false
    }

    private let lock = NSLock()
    private var world = World()

    func with<R>(_ body: (inout World) throws(CoreStatus) -> R) throws(CoreStatus) -> R {
        lock.lock()
        defer { lock.unlock() }
        return try body(&world)
    }

    func locked<R>(_ body: (inout World) -> R) -> R {
        lock.lock()
        defer { lock.unlock() }
        return body(&world)
    }
}

extension FakeCore {
    // MARK: Helpers

    static let day: Int64 = 86_400

    private static func json(_ object: Any) -> String {
        let data =
            (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    private static func object(_ text: String) throws(CoreStatus) -> [String: Any] {
        guard let o = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] else {
            throw .invalidArgument
        }
        return o
    }

    private static func camera(_ any: Any?) throws(CoreStatus) -> RecordingCamera {
        guard let o = any as? [String: Any], let device = o["device"] as? String, let channel = o["channel"] as? Int,
            (0...255).contains(channel)
        else { throw .invalidArgument }
        let lens = o["lens"] as? Int ?? 0
        guard (0...1).contains(lens) else { throw .invalidArgument }
        return RecordingCamera(device: device, channel: channel, lens: lens)
    }

    private static func cameraObject(_ c: RecordingCamera) -> [String: Any] {
        ["device": c.device, "channel": c.channel, "lens": c.lens]
    }

    private struct FakeEvent {
        var camera: RecordingCamera
        var kind: String
        var start: Int64
        var end: Int64
        var key: String { "i-\(start)-\(camera.device)-\(camera.channel)-\(camera.lens)" }
        /// The content hash's stand-in: the same in every run.
        var id: String {
            var h: UInt64 = 0xcbf2_9ce4_8422_2325
            for b in key.utf8 { h = (h ^ UInt64(b)) &* 0x100_0000_01b3 }
            return "\(start)-\(String(h, radix: 16))"
        }
    }

    /// The made-up events of an indexed camera-day.
    private static func events(_ camera: RecordingCamera, day: Int64) -> [FakeEvent] {
        [
            FakeEvent(camera: camera, kind: "people", start: day + 8 * 3600 + Int64(camera.channel), end: day + 8 * 3600 + 15),
            FakeEvent(camera: camera, kind: "vehicle", start: day + 17 * 3600 + Int64(camera.channel), end: day + 17 * 3600 + 20),
        ]
    }

    private func recordingsWorld<R>(_ body: (inout FakeRecordingsState.World) throws(CoreStatus) -> R) throws(CoreStatus) -> R {
        if isFreed { throw .closed }
        return try recordings.with(body)
    }

    private func configuredWorld<R>(_ body: (inout FakeRecordingsState.World) throws(CoreStatus) -> R) throws(CoreStatus) -> R {
        try recordingsWorld { w throws(CoreStatus) -> R in
            guard w.configured else { throw .notConfigured }
            return try body(&w)
        }
    }

    /// The cameras of the device a request names, checked against what is enabled.
    private static func allowed(_ w: FakeRecordingsState.World, _ c: RecordingCamera) -> Bool {
        guard let entry = w.enabled[c.device] else { return false }
        guard let list = entry else { return c.lens == 0 }
        return list.contains(c)
    }

    private static func requestCameras(_ o: [String: Any], _ w: FakeRecordingsState.World) throws(CoreStatus)
        -> [RecordingCamera]
    {
        guard let list = o["cameras"] as? [Any] else { throw .invalidArgument }
        var out: [RecordingCamera] = []
        for item in list {
            let c = try camera(item)
            guard allowed(w, c) else { throw .invalidArgument }
            if !out.contains(c) { out.append(c) }
        }
        return out
    }

    private struct Query {
        var start: Int64
        var end: Int64
        var cameras: [RecordingCamera]?
        var kinds: Set<String>?
        var limit: Int
    }

    private static func query(_ text: String) throws(CoreStatus) -> Query {
        let o = try object(text)
        guard let start = (o["start"] as? NSNumber)?.int64Value, let end = (o["end"] as? NSNumber)?.int64Value,
            end >= start
        else { throw .invalidArgument }
        var cameras: [RecordingCamera]?
        if let list = o["cameras"] as? [Any] {
            var cs: [RecordingCamera] = []
            for item in list { cs.append(try camera(item)) }
            cameras = cs
        }
        let kinds = (o["kinds"] as? [String]).map(Set.init)
        let limit = min(max(o["limit"] as? Int ?? 5000, 0), 100_000)
        return Query(start: start, end: end, cameras: cameras, kinds: kinds, limit: limit)
    }

    /// Every indexed event matching a query, by start.
    private static func matching(_ w: FakeRecordingsState.World, _ q: Query) -> [FakeEvent] {
        var out: [FakeEvent] = []
        for (device, days) in w.indexed {
            guard let entry = w.enabled[device] else { continue }
            let cams = entry ?? (0..<4).map { RecordingCamera(device: device, channel: $0) }
            for c in cams where q.cameras.map({ $0.contains(c) }) ?? true {
                for d in days.sorted() {
                    for e in events(c, day: d) where e.end >= q.start && e.start <= q.end {
                        if let kinds = q.kinds, !kinds.contains(e.kind) { continue }
                        out.append(e)
                    }
                }
            }
        }
        return out.sorted { ($0.start, $0.key) < ($1.start, $1.key) }
    }

    private static func eventObject(_ e: FakeEvent) -> [String: Any] {
        ["camera": cameraObject(e.camera), "kind": e.kind, "source": "device", "start": e.start, "end": e.end]
    }

    private static func incidentObject(_ w: FakeRecordingsState.World, _ e: FakeEvent) -> [String: Any] {
        let name: Any = w.names[e.key] ?? NSNull()
        let downloads = w.downloads.filter { $0.incident == e.key }.map(\.id)
        return [
            "key": e.key, "id": e.id, "name": name, "group": NSNull(), "start": e.start, "end": e.end,
            "margin_s": 5, "kinds": [e.kind], "growing": false, "edited": w.names[e.key] != nil,
            "locked": w.locks.contains(e.key),
            "cameras": [
                [
                    "camera": cameraObject(e.camera), "name": "Camera \(e.camera.channel + 1)",
                    "intervals": [[e.start, e.end]], "download": [e.start - 5, e.end + 5], "added": false,
                ] as [String: Any]
            ],
            "events": [eventObject(e)], "downloads": downloads,
        ]
    }

    private static func allIncidents(_ w: FakeRecordingsState.World) -> [FakeEvent] {
        matching(w, Query(start: 0, end: .max, cameras: nil, kinds: nil, limit: .max))
    }

    // MARK: Configuration and the index

    public func recordingsConfigure(json: String) throws(CoreStatus) {
        let o = try Self.object(json)
        if let d = o["downloads_dir"], !(d is String) { throw .invalidArgument }
        try recordingsWorld { w throws(CoreStatus) in
            w.configured = true
            w.downloadsDir = o["downloads_dir"] as? String
        }
    }

    public func recordingsEnable(deviceKey: String, json: String?) throws(CoreStatus) {
        guard !deviceKey.isEmpty, deviceKey.utf8.count <= 256 else { throw .invalidArgument }
        var cameras: [RecordingCamera]?
        if let json {
            let o = try Self.object(json)
            if let list = o["channels"] as? [Any] {
                var cs: [RecordingCamera] = []
                for item in list {
                    guard let c = item as? [String: Any], let ch = c["channel"] as? Int, (0...255).contains(ch) else {
                        throw .invalidArgument
                    }
                    cs.append(RecordingCamera(device: deviceKey, channel: ch, lens: c["lens"] as? Int ?? 0))
                }
                cameras = cs
            } else if let s = o["channels"] as? String {
                guard s == "all" else { throw .invalidArgument }
            }
        }
        try configuredWorld { w throws(CoreStatus) in
            if json == nil {
                w.enabled[deviceKey] = nil
                for hold in w.holds.keys { w.holds[hold]?.removeAll { $0.device == deviceKey } }
            } else {
                w.enabled[deviceKey] = .some(cameras)
            }
        }
    }

    public func recordingsWant(json: String) throws(CoreStatus) {
        let o = try Self.object(json)
        guard let rawDays = o["days"] as? [Any] else { throw .invalidArgument }
        var days: Set<Int64> = []
        for d in rawDays {
            guard let s = (d as? NSNumber)?.int64Value, s >= 0 else { throw .invalidArgument }
            days.insert(s - s % Self.day)
        }
        guard days.count <= 31 else { throw .limit }
        let changed = try configuredWorld { w throws(CoreStatus) -> [(String, Int64, Int64, Int)] in
            let cams = try Self.requestCameras(o, w)
            var out: [(String, Int64, Int64, Int)] = []
            for device in Set(cams.map(\.device)).sorted() {
                w.indexed[device, default: []].formUnion(days)
                w.revision += 1
                out.append((device, days.min() ?? 0, (days.max() ?? 0) + Self.day - 1, w.revision))
            }
            if !out.isEmpty { w.incidentsRevision += 1 }
            return out
        }
        for (device, start, end, revision) in changed {
            emit(
                event: Self.json([
                    "type": "recordings_index", "device": device, "state": "busy", "pending_days": days.count,
                    "revision": revision - 1,
                ]))
            emit(
                event: Self.json([
                    "type": "recordings_changed", "device": device, "revision": revision, "start": start, "end": end,
                ]))
            emit(
                event: Self.json([
                    "type": "recordings_index", "device": device, "state": "idle", "pending_days": 0,
                    "revision": revision,
                ]))
        }
        if !changed.isEmpty {
            let r = (try? recordingsWorld { $0.incidentsRevision }) ?? 0
            emit(event: Self.json(["type": "incidents_changed", "revision": r]))
        }
    }

    public func recordingsHoldToday(_ hold: UInt64, json: String?) throws(CoreStatus) {
        guard hold != 0 else { throw .invalidArgument }
        let o = try json.map { (j: String) throws(CoreStatus) -> [String: Any] in try Self.object(j) }
        try configuredWorld { w throws(CoreStatus) in
            guard let o else {
                w.holds[hold] = nil
                return
            }
            w.holds[hold] = try Self.requestCameras(o, w)
        }
    }

    /// The cameras each hold keeps fresh (for tests).
    public var recordingsHolds: [UInt64: [RecordingCamera]] {
        (try? recordingsWorld { $0.holds }) ?? [:]
    }

    public func recordingsStatus() throws(CoreStatus) -> String {
        try configuredWorld { w throws(CoreStatus) -> String in
            let devices = w.enabled.keys.sorted().map { k -> [String: Any] in
                ["device": k, "state": "idle", "pending_days": 0, "revision": w.revision]
            }
            return Self.json(["revision": w.revision, "devices": devices])
        }
    }

    public func recordingsCalendar(query: String) throws(CoreStatus) -> String {
        let q = try Self.query(query)
        return try configuredWorld { w throws(CoreStatus) -> String in
            var lists: [[String: Any]] = []
            for (device, days) in w.indexed.sorted(by: { $0.key < $1.key }) {
                guard let entry = w.enabled[device] else { continue }
                let cams = entry ?? (0..<4).map { RecordingCamera(device: device, channel: $0) }
                for c in cams where q.cameras.map({ $0.contains(c) }) ?? true {
                    let inRange = days.filter { $0 + Self.day - 1 >= q.start && $0 <= q.end }.sorted()
                    if !inRange.isEmpty { lists.append(["camera": Self.cameraObject(c), "days": inRange]) }
                }
            }
            let truncated = lists.count > q.limit
            return Self.json(["revision": w.revision, "truncated": truncated, "days": Array(lists.prefix(q.limit))])
        }
    }

    public func recordingsSpans(query: String) throws(CoreStatus) -> String {
        let q = try Self.query(query)
        return try configuredWorld { w throws(CoreStatus) -> String in
            var lanes: [[String: Any]] = []
            var count = 0
            var truncated = false
            for (device, days) in w.indexed.sorted(by: { $0.key < $1.key }) {
                guard let entry = w.enabled[device] else { continue }
                let cams = entry ?? (0..<4).map { RecordingCamera(device: device, channel: $0) }
                for c in cams where q.cameras.map({ $0.contains(c) }) ?? true {
                    var spans: [[String: Any]] = []
                    for d in days.sorted() {
                        for h in Int64(0)..<24 {
                            let s = d + h * 3600
                            guard s + 3599 >= q.start, s <= q.end else { continue }
                            guard count < q.limit else {
                                truncated = true
                                continue
                            }
                            count += 1
                            spans.append([
                                "start": s, "end": s + 3599, "bytes": 1 << 28, "causes": ["sched"], "tier": "main",
                            ])
                        }
                    }
                    if !spans.isEmpty { lanes.append(["camera": Self.cameraObject(c), "spans": spans]) }
                }
            }
            return Self.json(["revision": w.revision, "truncated": truncated, "spans": lanes])
        }
    }

    public func recordingsEvents(query: String) throws(CoreStatus) -> String {
        let q = try Self.query(query)
        return try configuredWorld { w throws(CoreStatus) -> String in
            let all = Self.matching(w, q)
            return Self.json([
                "revision": w.revision, "truncated": all.count > q.limit,
                "events": all.prefix(q.limit).map(Self.eventObject),
            ])
        }
    }

    // MARK: Incidents, locks, groups

    public func incidentsGet(query: String) throws(CoreStatus) -> String {
        let q = try Self.query(query)
        return try configuredWorld { w throws(CoreStatus) -> String in
            let all = Array(Self.matching(w, q).reversed())
            return Self.json([
                "revision": w.incidentsRevision, "truncated": all.count > q.limit,
                "incidents": all.prefix(q.limit).map { Self.incidentObject(w, $0) },
            ])
        }
    }

    public func incidentGet(key: String) throws(CoreStatus) -> String {
        try configuredWorld { w throws(CoreStatus) -> String in
            guard let e = Self.allIncidents(w).first(where: { $0.key == key }) else { throw .unknownID }
            return Self.json(["revision": w.incidentsRevision, "incident": Self.incidentObject(w, e)])
        }
    }

    public func incidentEdit(json: String) throws(CoreStatus) -> String {
        let o = try Self.object(json)
        guard let op = o["op"] as? String else { throw .invalidArgument }
        let revision = try configuredWorld { w throws(CoreStatus) -> (Int, String) in
            guard op == "rename" || op == "revert" else { throw .unsupported }
            guard let key = o["key"] as? String else { throw .invalidArgument }
            guard Self.allIncidents(w).contains(where: { $0.key == key }) else { throw .unknownID }
            if op == "rename", let name = o["name"] as? String {
                guard name.utf8.count <= 256 else { throw .invalidArgument }
                w.names[key] = name
            } else {
                w.names[key] = nil
            }
            w.incidentsRevision += 1
            return (w.incidentsRevision, key)
        }
        emit(event: Self.json(["type": "incidents_changed", "revision": revision.0]))
        return Self.json(["revision": revision.0, "keys": [revision.1]])
    }

    public func incidentLock(key: String, locked: Bool) throws(CoreStatus) -> String {
        let (answer, queued, revision) = try configuredWorld {
            w throws(CoreStatus) -> ([String: Any], FakeRecordingsState.Download?, Int?) in
            guard let e = Self.allIncidents(w).first(where: { $0.key == key }) else { throw .unknownID }
            let changed = locked ? w.locks.insert(key).inserted : w.locks.remove(key) != nil
            if changed { w.incidentsRevision += 1 }
            var answer: [String: Any] = ["revision": w.incidentsRevision, "keys": changed ? [key] : []]
            guard locked else { return (answer, nil, changed ? w.incidentsRevision : nil) }
            var queued: FakeRecordingsState.Download?
            if w.downloadsDir == nil {
                answer["download"] = NSNull()
                answer["folder"] = NSNull()
                answer["download_status"] = "not_configured"
            } else if let existing = w.downloads.first(where: { $0.incident == key }) {
                answer["download"] = existing.id
                answer["folder"] = existing.folder
                answer["download_status"] = "existing"
            } else {
                let d = Self.newDownload(&w, e, cameras: [e.camera])
                queued = d
                answer["download"] = d.id
                answer["folder"] = d.folder
                answer["download_status"] = "queued"
            }
            return (answer, queued, changed ? w.incidentsRevision : nil)
        }
        if let revision { emit(event: Self.json(["type": "incidents_changed", "revision": revision])) }
        if let queued { downloadQueued(queued) }
        return Self.json(answer)
    }

    public func cameraGroupsGet() throws(CoreStatus) -> String {
        try configuredWorld { w throws(CoreStatus) -> String in
            var o = (w.groups as? [String: Any]) ?? [:]
            o["revision"] = w.groupsRevision
            o["groups"] = o["groups"] ?? [Any]()
            o["camera_names"] = o["camera_names"] ?? [Any]()
            return Self.json(o)
        }
    }

    public func cameraGroupsPut(json: String) throws(CoreStatus) {
        let o = try Self.object(json)
        let groups = o["groups"] as? [Any] ?? []
        let names = o["camera_names"] as? [Any] ?? []
        guard groups.count <= 64, names.count <= 4096 else { throw .limit }
        let revisions = try configuredWorld { w throws(CoreStatus) -> (Int, Int) in
            w.groups = ["groups": groups, "camera_names": names]
            w.groupsRevision += 1
            w.incidentsRevision += 1
            return (w.groupsRevision, w.incidentsRevision)
        }
        emit(event: Self.json(["type": "camera_groups_changed", "revision": revisions.0]))
        emit(event: Self.json(["type": "incidents_changed", "revision": revisions.1]))
    }

    // MARK: Thumbnails

    private static func thumbnailKey(_ json: String) throws(CoreStatus) -> ThumbnailKey {
        guard let key = try? JSONDecoder().decode(ThumbnailKey.self, from: Data(json.utf8)), key.end >= key.start,
            key.tier == "main" || key.tier == "sub"
        else { throw .invalidArgument }
        return key
    }

    /// A stand-in H.264 keyframe (not decodable): the app's decoder fails on
    /// it, as on any thumbnail it cannot read, and the view keeps its
    /// placeholder.
    public static let standInKeyframe = Data([0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88])

    public func thumbnailGet(key: String) throws(CoreStatus) -> ThumbnailResult {
        let k = try Self.thumbnailKey(key)
        let cached = try configuredWorld { w throws(CoreStatus) -> Thumbnail? in
            if let t = w.thumbnails[k] { return t }
            if !w.pendingThumbnails.contains(k) { w.pendingThumbnails.append(k) }
            return nil
        }
        if let cached { return .ready(cached) }
        scheduleRecordingWork()
        return .pending
    }

    public func thumbnailPut(key: String, thumbnail: Thumbnail) throws(CoreStatus) {
        let k = try Self.thumbnailKey(key)
        guard thumbnail.data.count <= 64 << 20 else { throw .limit }
        try configuredWorld { w throws(CoreStatus) in w.thumbnails[k] = thumbnail }
    }

    public func thumbnailCancel(key: String?) throws(CoreStatus) {
        let k = try key.map { (j: String) throws(CoreStatus) -> ThumbnailKey in try Self.thumbnailKey(j) }
        try configuredWorld { w throws(CoreStatus) in
            if let k { w.pendingThumbnails.removeAll { $0 == k } } else { w.pendingThumbnails.removeAll() }
        }
    }

    // MARK: Downloads

    private static func newDownload(
        _ w: inout FakeRecordingsState.World, _ e: FakeEvent, cameras: [RecordingCamera]
    ) -> FakeRecordingsState.Download {
        let id = "\(e.id)-\(w.nextDownload)"
        w.nextDownload += 1
        let folder = (w.downloadsDir ?? "") + "/\(e.start) \(e.kind) — Camera \(e.camera.channel + 1)"
        let d = FakeRecordingsState.Download(
            id: id, incident: e.key, folder: folder, cameras: cameras, state: "queued", paused: false)
        w.downloads.append(d)
        return d
    }

    private static func downloadObject(_ d: FakeRecordingsState.Download) -> [String: Any] {
        let done = d.state == "done"
        let cameras = d.cameras.map { c -> [String: Any] in
            [
                "camera": cameraObject(c), "name": "Camera \(c.channel + 1)", "state": d.state, "paused": d.paused,
                "chunks_done": done ? 1 : 0, "chunks_total": 1, "bytes": done ? 1 << 20 : 0,
                "output": done ? "\(d.folder)/Camera \(c.channel + 1).mp4" as Any : NSNull(),
            ]
        }
        return [
            "download": d.id, "incident": d.incident, "folder": d.folder, "state": d.paused ? "paused" : d.state,
            "paused": d.paused, "cameras": cameras,
        ]
    }

    private func downloadQueued(_ d: FakeRecordingsState.Download) {
        var o = Self.downloadObject(d)
        o["type"] = "download"
        emit(event: Self.json(o))
        scheduleRecordingWork()
    }

    public func downloadIncident(json: String) throws(CoreStatus) -> String {
        let o = try Self.object(json)
        guard let key = o["incident"] as? String else { throw .invalidArgument }
        let (answer, queued) = try configuredWorld {
            w throws(CoreStatus) -> ([String: Any], FakeRecordingsState.Download?) in
            guard w.downloadsDir != nil else { throw .notConfigured }
            guard let e = Self.allIncidents(w).first(where: { $0.key == key }) else { throw .unknownID }
            if let existing = w.downloads.first(where: { $0.incident == key }) {
                return (["download": existing.id, "folder": existing.folder, "existing": true], nil)
            }
            guard w.downloads.count < 1000 else { throw .limit }
            var cameras = [e.camera]
            if let list = o["cameras"] as? [Any] {
                cameras = []
                for item in list {
                    let c = try Self.camera((item as? [String: Any])?["camera"])
                    guard c == e.camera else { throw .invalidArgument }
                    cameras.append(c)
                }
            }
            let d = Self.newDownload(&w, e, cameras: cameras)
            return (["download": d.id, "folder": d.folder, "existing": false], d)
        }
        if let queued { downloadQueued(queued) }
        return Self.json(answer)
    }

    private func changeDownload(_ id: String, _ body: (inout FakeRecordingsState.Download) throws(CoreStatus) -> Void)
        throws(CoreStatus)
    {
        let d = try configuredWorld { w throws(CoreStatus) -> FakeRecordingsState.Download in
            guard w.downloadsDir != nil else { throw .notConfigured }
            guard let i = w.downloads.firstIndex(where: { $0.id == id }) else { throw .unknownID }
            try body(&w.downloads[i])
            return w.downloads[i]
        }
        var o = Self.downloadObject(d)
        o["type"] = "download"
        emit(event: Self.json(o))
        scheduleRecordingWork()
    }

    public func downloadPause(_ download: String, camera: String?, paused: Bool) throws(CoreStatus) {
        let c = try camera.map { (j: String) throws(CoreStatus) -> RecordingCamera in
            try Self.camera(try Self.object(j))
        }
        try changeDownload(download) { d throws(CoreStatus) in
            if let c, !d.cameras.contains(c) { throw .unknownID }
            if d.state != "done" { d.paused = paused }
        }
    }

    public func downloadRetry(_ download: String, camera: String?) throws(CoreStatus) {
        let c = try camera.map { (j: String) throws(CoreStatus) -> RecordingCamera in
            try Self.camera(try Self.object(j))
        }
        try changeDownload(download) { d throws(CoreStatus) in
            if let c, !d.cameras.contains(c) { throw .unknownID }
            if d.state == "failed" { d.state = "queued" }
        }
    }

    public func downloadForget(_ download: String, deletePartial: Bool) throws(CoreStatus) {
        try configuredWorld { w throws(CoreStatus) in
            guard w.downloadsDir != nil else { throw .notConfigured }
            guard let i = w.downloads.firstIndex(where: { $0.id == download }) else { throw .unknownID }
            let d = w.downloads[i]
            if (d.state == "queued" || d.state == "running") && !d.paused { throw .invalidArgument }
            w.downloads.remove(at: i)
        }
        emit(event: Self.json(["type": "download_removed", "download": download, "trash": false]))
    }

    public func downloadTrash(_ download: String) throws(CoreStatus) -> String {
        let folder = try configuredWorld { w throws(CoreStatus) -> String in
            guard w.downloadsDir != nil else { throw .notConfigured }
            guard let i = w.downloads.firstIndex(where: { $0.id == download }) else { throw .unknownID }
            return w.downloads.remove(at: i).folder
        }
        emit(event: Self.json(["type": "download_removed", "download": download, "trash": true, "folder": folder]))
        return Self.json(["folder": folder])
    }

    public func downloadsGet() throws(CoreStatus) -> String {
        try configuredWorld { w throws(CoreStatus) -> String in
            guard w.downloadsDir != nil else { throw .notConfigured }
            return Self.json(["downloads": w.downloads.map(Self.downloadObject)])
        }
    }

    // MARK: Sound settings

    public func soundConfigure(json: String) throws(CoreStatus) {
        let o = try Self.object(json)
        if let a = o["alarms"], !(a is Bool) { throw .invalidArgument }
        if let h = o["loudness_history_s"] {
            guard let n = h as? Int, (0...3600).contains(n) else { throw .invalidArgument }
        }
        if isFreed { throw .closed }
    }

    // MARK: Queued work

    private func scheduleRecordingWork() {
        guard !options.recordingWorkIsManual else { return }
        DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(20)) { [weak self] in
            self?.completeRecordingWork()
        }
    }

    /// Finishes every queued thumbnail fetch (`thumbnail` `ready` with a
    /// stand-in keyframe) and download (`running`, then `done`), sending
    /// their events. Paused downloads wait.
    public func completeRecordingWork() {
        guard !isFreed else { return }
        let (thumbs, downloads) = recordings.locked { w -> ([ThumbnailKey], [FakeRecordingsState.Download]) in
            let thumbs = w.pendingThumbnails
            w.pendingThumbnails.removeAll()
            for k in thumbs {
                w.thumbnails[k] = Thumbnail(
                    kind: .h264Keyframe, width: 640, height: 360, ptsMicros: 0, data: Self.standInKeyframe)
            }
            var done: [FakeRecordingsState.Download] = []
            for i in w.downloads.indices where !w.downloads[i].paused && w.downloads[i].state == "queued" {
                w.downloads[i].state = "done"
                done.append(w.downloads[i])
            }
            return (thumbs, done)
        }
        for k in thumbs {
            guard let keyJSON = try? JSONEncoder().encode(k),
                let key = try? JSONSerialization.jsonObject(with: keyJSON)
            else { continue }
            emit(event: Self.json(["type": "thumbnail", "key": key, "state": "ready"]))
        }
        for d in downloads {
            var running = Self.downloadObject(FakeRecordingsState.Download(
                id: d.id, incident: d.incident, folder: d.folder, cameras: d.cameras, state: "running", paused: false))
            running["type"] = "download"
            emit(event: Self.json(running))
            var o = Self.downloadObject(d)
            o["type"] = "download"
            emit(event: Self.json(o))
        }
    }
}
