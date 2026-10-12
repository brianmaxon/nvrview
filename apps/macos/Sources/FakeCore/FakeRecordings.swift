import Foundation
import NVRCore
import VideoKit

// Recordings, part 1 (minor 5) in the fake core: a made-up recording store
// with the contract's JSON (docs/FFI-RECORDINGS.md, sections 1 to 5 and 8),
// for the recordings views (milestone P, M1) before they meet the real core.
// No device and no files: every enabled camera recorded every day all day,
// with a person at 08:00 and a vehicle at 17:00 (device-local seconds), and
// on channel 3 (index 2) an animal (`dog_cat`) at 12:00. Each event is an
// incident of its own, except that events of cameras in one named camera
// group join when they overlap (within 10 s); with no groups every event
// stays alone (the real core then groups any cameras).
// Asking for days indexes them at once (`recordings_index` busy, then
// `recordings_changed`, `recordings_index` idle, `incidents_changed`).
// Thumbnail fetches and downloads are queued and finish shortly after the
// call (or only on `completeRecordingWork()` with
// `Options.recordingWorkIsManual`): a fetch returns the first keyframe of
// the first clip (a stand-in keyframe without clips), a download goes to
// `done` with an output path that is never written. Every edit applies
// (merge, split, rename, add and remove a camera, revert); stored edits are
// replayed in order over the incidents, and one whose keys match nothing is
// skipped. Locks mark incidents; they do not follow edits as the core's do.
// Sound labels (`nv_sound_labels_put`) are kept as events with `source`
// `local_audio` on indexed days, replacing the same analyzer's labels in
// the request's range, as the core's are.

/// What the fake core keeps for recordings, under one lock.
final class FakeRecordingsState: @unchecked Sendable {
    enum FakeEdit {
        case merge([String])
        case split(String, Int64)
        case addCamera(String, RecordingCamera)
        case removeCamera(String, RecordingCamera)

        var keys: [String] {
            switch self {
            case .merge(let k): k
            case .split(let k, _), .addCamera(let k, _), .removeCamera(let k, _): [k]
            }
        }
    }

    struct Download {
        var id: String
        var incident: String
        var folder: String
        var cameras: [RecordingCamera]
        var state: String
        var paused: Bool
        /// The incident's span (device-local seconds): what the download covers.
        var start: Int64 = 0
        var end: Int64 = 0
    }

    struct World {
        var configured = false
        var downloadsDir: String?
        /// Allowed cameras per device key (nil: every channel, first lens).
        var enabled: [String: [RecordingCamera]?] = [:]
        var indexed: [String: Set<Int64>] = [:]
        /// Days the calendar knows (the months of wanted days, and months
        /// asked for alone).
        var calendar: [String: Set<Int64>] = [:]
        var monthRequests = 0
        var revision = 0
        var incidentsRevision = 0
        var names: [String: String] = [:]
        var edits: [FakeEdit] = []
        var locks: Set<String> = []
        var groups: Any = [String: Any]()
        var groupsRevision = 0
        var holds: [UInt64: [RecordingCamera]] = [:]
        var thumbnails: [ThumbnailKey: Thumbnail] = [:]
        var pendingThumbnails: [ThumbnailKey] = []
        var downloads: [Download] = []
        var nextDownload = 1
        /// Cameras on the sound "always listen" list.
        var watched: [RecordingCamera] = []
        /// Sound labels the app put: camera, kind, span, analyzer.
        var labels: [(camera: RecordingCamera, kind: String, start: Int64, end: Int64, analyzer: String)] = []
        /// Sound analysis requests that have not finished: request, camera.
        var analyses: [(request: UInt64, camera: RecordingCamera, day: Int64)] = []
        /// Seconds of audio analyzed from downloads: first-lens camera, span.
        var analyzed: [(camera: RecordingCamera, start: Int64, end: Int64)] = []
        var nextAnalysis: UInt64 = 1
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
        var source = "device"
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
        ] + (camera.channel == 2
            ? [FakeEvent(camera: camera, kind: "dog_cat", start: day + 12 * 3600 + 30, end: day + 12 * 3600 + 45)] : [])
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
                for l in w.labels where l.camera == c && l.end >= q.start && l.start <= q.end {
                    guard days.contains(DeviceTime.dayStart(l.start)) else { continue }
                    if let kinds = q.kinds, !kinds.contains(l.kind) { continue }
                    out.append(FakeEvent(camera: c, kind: l.kind, start: l.start, end: l.end, source: "local_audio"))
                }
            }
        }
        return out.sorted { ($0.start, $0.key) < ($1.start, $1.key) }
    }

    private static func eventObject(_ e: FakeEvent) -> [String: Any] {
        ["camera": cameraObject(e.camera), "kind": e.kind, "source": e.source, "start": e.start, "end": e.end]
    }

    /// An incident of the fake: events (and cameras added by an edit).
    private struct FakeIncident {
        var events: [FakeEvent]
        var added: [RecordingCamera] = []
        var group: String?
        var edited = false

        var start: Int64 { events.map(\.start).min() ?? 0 }
        var end: Int64 { events.map(\.end).max() ?? 0 }
        var first: FakeEvent { events.min { ($0.start, $0.key) < ($1.start, $1.key) }! }
        var key: String { first.key + (group.map { "-g" + FakeCore.hash($0).prefix(8) } ?? "") }
        var cameras: [RecordingCamera] {
            var out: [RecordingCamera] = []
            for e in events.sorted(by: { ($0.start, $0.key) < ($1.start, $1.key) }) where !out.contains(e.camera) {
                out.append(e.camera)
            }
            for c in added where !out.contains(c) { out.append(c) }
            return out
        }
        var kinds: [String] {
            var out: [String] = []
            for e in events where !out.contains(e.kind) { out.append(e.kind) }
            return out
        }
        var id: String {
            let text = key + cameras.map { "\($0.device)/\($0.channel)/\($0.lens)" }.joined() + "\(start)-\(end)"
            return "\(start)-\(FakeCore.hash(text))"
        }
    }

    fileprivate static func hash(_ text: String) -> String {
        var h: UInt64 = 0xcbf2_9ce4_8422_2325
        for b in text.utf8 { h = (h ^ UInt64(b)) &* 0x100_0000_01b3 }
        return String(h, radix: 16)
    }

    private static func incidentObject(_ w: FakeRecordingsState.World, _ i: FakeIncident) -> [String: Any] {
        let name: Any = w.names[i.key] ?? NSNull()
        let downloads = w.downloads.filter { $0.incident == i.key }.map(\.id)
        let cameras = i.cameras.map { c -> [String: Any] in
            let own = i.events.filter { $0.camera == c }
            let intervals = own.isEmpty ? [[i.start, i.end]] : own.map { [$0.start, $0.end] }
            let lo = own.map(\.start).min() ?? i.start, hi = own.map(\.end).max() ?? i.end
            return [
                "camera": cameraObject(c), "name": "Camera \(c.channel + 1)", "intervals": intervals,
                "download": [lo - 5, hi + 5], "added": own.isEmpty,
            ]
        }
        return [
            "key": i.key, "id": i.id, "name": name, "group": i.group.map { $0 as Any } ?? NSNull(),
            "start": i.start, "end": i.end, "margin_s": 5, "kinds": i.kinds, "growing": false,
            "edited": i.edited || w.names[i.key] != nil, "locked": w.locks.contains(i.key), "cameras": cameras,
            "events": i.events.sorted { ($0.start, $0.key) < ($1.start, $1.key) }.map(eventObject),
            "downloads": downloads,
        ]
    }

    /// The named groups' cameras (the stored camera groups).
    private static func groupOf(_ w: FakeRecordingsState.World) -> [RecordingCamera: String] {
        var out: [RecordingCamera: String] = [:]
        let groups = (w.groups as? [String: Any])?["groups"] as? [[String: Any]] ?? []
        for g in groups {
            guard let name = g["name"] as? String else { continue }
            for item in g["cameras"] as? [Any] ?? [] {
                if let c = try? camera(item), out[c] == nil { out[c] = name }
            }
        }
        return out
    }

    /// Every incident: the events grouped, then the stored edits replayed.
    private static func allIncidents(_ w: FakeRecordingsState.World) -> [FakeIncident] {
        let groupOf = groupOf(w)
        var incidents: [FakeIncident] = []
        for e in matching(w, Query(start: 0, end: .max, cameras: nil, kinds: nil, limit: .max)) {
            let g = groupOf[e.camera]
            if let g, let i = incidents.lastIndex(where: { $0.group == g && e.start <= $0.end + 10 }) {
                incidents[i].events.append(e)
            } else {
                incidents.append(FakeIncident(events: [e], group: g))
            }
        }
        for edit in w.edits { try? apply(edit, to: &incidents, w) }
        return incidents.sorted { ($0.start, $0.key) < ($1.start, $1.key) }
    }

    /// Applies one edit; throws (changing nothing) when it cannot.
    private static func apply(
        _ edit: FakeRecordingsState.FakeEdit, to incidents: inout [FakeIncident], _ w: FakeRecordingsState.World
    ) throws(CoreStatus) {
        func index(_ key: String) throws(CoreStatus) -> Int {
            guard let i = incidents.firstIndex(where: { $0.key == key }) else { throw .unknownID }
            return i
        }
        switch edit {
        case .merge(let keys):
            guard Set(keys).count >= 2 else { throw .invalidArgument }
            guard keys.count <= 1000 else { throw .limit }
            var parts: [FakeIncident] = []
            for k in Set(keys) { parts.append(incidents[try index(k)]) }
            incidents.removeAll { i in parts.contains { $0.key == i.key } }
            var merged = FakeIncident(
                events: parts.flatMap(\.events), added: parts.flatMap(\.added),
                group: Set(parts.map(\.group)).count == 1 ? parts[0].group : nil, edited: true)
            merged.added.removeAll { c in merged.events.contains { $0.camera == c } }
            incidents.append(merged)
        case .split(let key, let at):
            let i = try index(key)
            let a = incidents[i].events.filter { $0.start < at }, b = incidents[i].events.filter { $0.start >= at }
            guard !a.isEmpty, !b.isEmpty else { throw .invalidArgument }
            let old = incidents.remove(at: i)
            incidents.append(FakeIncident(events: a, added: old.added, group: old.group, edited: true))
            incidents.append(FakeIncident(events: b, group: old.group, edited: true))
        case .addCamera(let key, let c):
            let i = try index(key)
            guard allowed(w, c) else { throw .invalidArgument }
            guard !incidents[i].cameras.contains(c) else { return }
            incidents[i].added.append(c)
            incidents[i].edited = true
        case .removeCamera(let key, let c):
            let i = try index(key)
            guard incidents[i].cameras.contains(c), incidents[i].cameras.count > 1 else { throw .invalidArgument }
            incidents[i].added.removeAll { $0 == c }
            incidents[i].events.removeAll { $0.camera == c }
            incidents[i].edited = true
        }
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

    /// Every day of the months that hold `seconds`, up to today on this
    /// computer's clock (the fake records every day all day).
    private static func calendarDays(_ seconds: Set<Int64>) -> Set<Int64> {
        let today = DeviceTime.today(now: Date(), utcOffset: TimeZone.current.secondsFromGMT())
        var out: Set<Int64> = []
        for s in seconds {
            let m = DeviceTime.month(containing: s)
            for d in stride(from: m.start, to: m.next, by: Int(Self.day)) where d <= today { out.insert(d) }
        }
        return out
    }

    public func recordingsWant(json: String) throws(CoreStatus) {
        let o = try Self.object(json)
        let rawDays = o["days"] as? [Any], rawMonths = o["months"] as? [Any]
        guard rawDays != nil || rawMonths != nil else { throw .invalidArgument }
        var days: Set<Int64> = [], months: Set<Int64> = []
        for d in rawDays ?? [] {
            guard let s = (d as? NSNumber)?.int64Value, s >= 0 else { throw .invalidArgument }
            days.insert(s - s % Self.day)
        }
        for m in rawMonths ?? [] {
            guard let s = (m as? NSNumber)?.int64Value, s >= 0 else { throw .invalidArgument }
            months.insert(DeviceTime.month(containing: s).start)
        }
        guard days.count <= 31, months.count <= 12 else { throw .limit }
        // A day's month calendar comes with it, as on the core; months alone
        // (minor 7) bring only their calendars.
        let calendar = Self.calendarDays(days.union(months))
        let changed = try configuredWorld { w throws(CoreStatus) -> [(String, Int64, Int64, Int)] in
            let cams = try Self.requestCameras(o, w)
            var out: [(String, Int64, Int64, Int)] = []
            for device in Set(cams.map(\.device)).sorted() {
                w.indexed[device, default: []].formUnion(days)
                w.calendar[device, default: []].formUnion(calendar)
                w.monthRequests += months.count
                w.revision += 1
                let all = days.union(months.flatMap { [$0, DeviceTime.month(containing: $0).next - 1] })
                out.append((device, all.min() ?? 0, DeviceTime.dayStart(all.max() ?? 0) + Self.day - 1, w.revision))
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

    /// The cameras on the sound "always listen" list (for tests).
    public var soundWatched: [RecordingCamera] { (try? recordingsWorld { $0.watched }) ?? [] }

    /// How many months were asked for alone, per device and in all (for tests).
    public var recordingsMonthRequests: Int {
        (try? recordingsWorld { $0.monthRequests }) ?? 0
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
            for device in Set(w.indexed.keys).union(w.calendar.keys).sorted() {
                guard let entry = w.enabled[device] else { continue }
                let days = (w.indexed[device] ?? []).union(w.calendar[device] ?? [])
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
        let group = (try? Self.object(query))?["group"] as? String
        return try configuredWorld { w throws(CoreStatus) -> String in
            let all = Array(
                Self.allIncidents(w).filter { i in
                    i.end >= q.start && i.start <= q.end
                        && (q.cameras.map { cs in i.cameras.contains { cs.contains($0) } } ?? true)
                        && (q.kinds.map { ks in i.kinds.contains { ks.contains($0) } } ?? true)
                        && (group.map { i.group == $0 } ?? true)
                }.reversed())
            return Self.json([
                "revision": w.incidentsRevision, "truncated": all.count > q.limit,
                "incidents": all.prefix(q.limit).map { Self.incidentObject(w, $0) },
            ])
        }
    }

    public func incidentGet(key: String) throws(CoreStatus) -> String {
        try configuredWorld { w throws(CoreStatus) -> String in
            guard let i = Self.allIncidents(w).first(where: { $0.key == key }) else { throw .unknownID }
            return Self.json(["revision": w.incidentsRevision, "incident": Self.incidentObject(w, i)])
        }
    }

    public func incidentEdit(json: String) throws(CoreStatus) -> String {
        let o = try Self.object(json)
        guard let op = o["op"] as? String else { throw .invalidArgument }
        let (revision, keys) = try configuredWorld { w throws(CoreStatus) -> (Int, [String]) in
            var incidents = Self.allIncidents(w)
            func key() throws(CoreStatus) -> String {
                guard let k = o["key"] as? String else { throw .invalidArgument }
                guard incidents.contains(where: { $0.key == k }) else { throw .unknownID }
                return k
            }
            var keys: [String]
            switch op {
            case "rename":
                let k = try key()
                if let name = o["name"] as? String {
                    guard name.utf8.count <= 256 else { throw .invalidArgument }
                    w.names[k] = name
                } else {
                    w.names[k] = nil
                }
                keys = [k]
            case "revert":
                let k = try key()
                // Every stored edit that named this incident or one of the
                // event incidents it is made of.
                let base = Set(incidents.first { $0.key == k }!.events.map(\.key))
                w.edits.removeAll { e in e.keys.contains { k2 in k2 == k || base.contains { k2.hasPrefix($0) } } }
                w.names[k] = nil
                keys = [k]
            default:
                let edit: FakeRecordingsState.FakeEdit
                switch op {
                case "merge":
                    guard let ks = o["keys"] as? [String] else { throw .invalidArgument }
                    edit = .merge(ks)
                case "split":
                    guard let at = (o["at"] as? NSNumber)?.int64Value else { throw .invalidArgument }
                    edit = .split(try key(), at)
                case "add_camera": edit = .addCamera(try key(), try Self.camera(o["camera"]))
                case "remove_camera": edit = .removeCamera(try key(), try Self.camera(o["camera"]))
                default: throw .invalidArgument
                }
                guard w.edits.count < 10_000 else { throw .limit }
                try Self.apply(edit, to: &incidents, w)
                w.edits.append(edit)
                switch edit {
                case .addCamera(let k, _), .removeCamera(let k, _): keys = [k]
                case .merge: keys = incidents.suffix(1).map(\.key)
                case .split: keys = incidents.suffix(2).map(\.key)
                }
            }
            w.incidentsRevision += 1
            return (w.incidentsRevision, keys)
        }
        emit(event: Self.json(["type": "incidents_changed", "revision": revision]))
        return Self.json(["revision": revision, "keys": keys])
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
                let d = Self.newDownload(&w, e, cameras: e.cameras)
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
        _ w: inout FakeRecordingsState.World, _ e: FakeIncident, cameras: [RecordingCamera]
    ) -> FakeRecordingsState.Download {
        let id = "\(e.id)-\(w.nextDownload)"
        w.nextDownload += 1
        let names = e.cameras.map { "Camera \($0.channel + 1)" }.joined(separator: ", ")
        let folder = (w.downloadsDir ?? "") + "/\(e.start) \(e.kinds.first ?? "recording") — \(names)"
        let d = FakeRecordingsState.Download(
            id: id, incident: e.key, folder: folder, cameras: cameras, state: "queued", paused: false, start: e.start,
            end: e.end)
        w.downloads.append(d)
        return d
    }

    private static func downloadObject(_ d: FakeRecordingsState.Download) -> [String: Any] {
        let done = d.state == "done"
        let cameras = d.cameras.map { c -> [String: Any] in
            [
                "camera": cameraObject(c), "name": "Camera \(c.channel + 1)", "state": d.state, "paused": d.paused,
                "chunks_done": done ? 1 : 0, "chunks_total": 1, "bytes": done ? 1 << 20 : 0,
                // As the core: null until the device gave an estimate.
                "bytes_estimate": NSNull(),
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
            var cameras = e.cameras
            if let list = o["cameras"] as? [Any] {
                cameras = []
                for item in list {
                    let c = try Self.camera((item as? [String: Any])?["camera"])
                    guard e.cameras.contains(c) else { throw .invalidArgument }
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

    /// The keyframe a thumbnail fetch returns: the first clip's first
    /// keyframe with its parameter sets (decodable), else the stand-in.
    var coverKeyframe: Thumbnail {
        guard let clip = options.clips.first, let unit = clip.units.first(where: \.isKeyframe) else {
            return Thumbnail(kind: .h264Keyframe, width: 640, height: 360, ptsMicros: 0, data: Self.standInKeyframe)
        }
        var data = clip.data.subdata(in: unit.range)
        let sets = { (d: Data) in
            AnnexB.split(d, codec: clip.codec).filter { NALType.isParameterSet($0.type, codec: clip.codec) }
        }
        if sets(data).isEmpty, let first = clip.units.first {
            let au = clip.data.subdata(in: first.range)
            for nal in sets(au).reversed() { data = Data([0, 0, 0, 1]) + au.subdata(in: nal.range) + data }
        }
        return Thumbnail(
            kind: clip.codec == .h265 ? .h265Keyframe : .h264Keyframe, width: 0, height: 0, ptsMicros: 0, data: data)
    }

    /// Finishes every queued thumbnail fetch (`thumbnail` `ready` with
    /// `coverKeyframe`) and download (`running`, then `done`), sending
    /// their events. Paused downloads wait.
    public func completeRecordingWork() {
        guard !isFreed else { return }
        defer { completeSoundAnalyses() }
        let cover = coverKeyframe
        let (thumbs, downloads) = recordings.locked { w -> ([ThumbnailKey], [FakeRecordingsState.Download]) in
            let thumbs = w.pendingThumbnails
            w.pendingThumbnails.removeAll()
            for k in thumbs {
                w.thumbnails[k] = cover
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

// MARK: Edit lists and sound (minor 8)
//
// `editCompute` plans a follow cut or a grid over a finished download of the
// made-up store: every camera of the download is a source of 1920 by 1080
// h264 that starts at the download range and has a keyframe every 2 s. The
// follow cut shows each event's camera while it lasts. `soundWatch` keeps a
// list and sends `sound_watch` events; loudness lanes are `null` except for
// watched cameras (a steady -40 dBFS peak, -50 RMS); `soundLabelsPut` only
// validates; `soundAnalyzeDay` queues a request that `completeRecordingWork`
// finishes with a `sound_analyze` `running` and `done` (no seconds, no
// labels). `tileLoudness` is in FakeCore.swift.

extension FakeCore {
    private static func requireKnownCamera(_ w: FakeRecordingsState.World, _ c: RecordingCamera) throws(CoreStatus) {
        guard allowed(w, c) else { throw .invalidArgument }
    }

    public func editCompute(json: String) throws(CoreStatus) -> String {
        let o = try Self.object(json)
        guard let kind = o["kind"] as? String, let id = o["download"] as? String else { throw .invalidArgument }
        switch kind {
        case "follow", "grid": break
        case "trim": throw .unsupported
        default: throw .invalidArgument
        }
        if o["in_ms"] != nil || o["out_ms"] != nil { throw .unsupported }
        let plan = try configuredWorld { w throws(CoreStatus) -> [String: Any] in
            guard w.downloadsDir != nil else { throw .notConfigured }
            guard let d = w.downloads.first(where: { $0.id == id }) else { throw .unknownID }
            guard d.state == "done",
                let inc = Self.allIncidents(w).first(where: { $0.key == d.incident })
            else { throw .invalidArgument }
            let cameras = d.cameras.filter { inc.cameras.contains($0) }
            guard !cameras.isEmpty, !inc.events.isEmpty else { throw .invalidArgument }
            let range = { (c: RecordingCamera) -> (Int64, Int64) in
                let own = inc.events.filter { $0.camera == c }
                return (
                    (own.map(\.start).min() ?? inc.start) - 5, (own.map(\.end).max() ?? inc.end) + 5
                )
            }
            let sources = cameras.enumerated().map { (i, c) -> [String: Any] in
                let start = range(c).0 * 1000
                let ends = range(c).1 * 1000
                return [
                    "source": i, "camera": Self.cameraObject(c), "name": "Camera \(c.channel + 1)",
                    "file": "\(d.folder)/Camera \(c.channel + 1).mp4", "file_start_ms": start, "offset_ms": 0,
                    "codec": "h264", "width": 1920, "height": 1080, "duration_ms": ends - start,
                ]
            }
            var plan: [String: Any] = [
                "version": 1, "kind": kind, "download": id, "sources": sources,
                "suggested_file": "\(d.folder)/\(inc.start) \(kind).mp4",
            ]
            if kind == "follow" {
                var segments: [[String: Any]] = []
                for e in inc.events.sorted(by: { ($0.start, $0.key) < ($1.start, $1.key) }) {
                    guard let i = cameras.firstIndex(of: e.camera) else { continue }
                    let from = max(e.start * 1000, segments.last.map { $0["end_ms"] as! Int64 } ?? 0)
                    guard from < e.end * 1000 else { continue }
                    if let last = segments.last, last["source"] as? Int == i {
                        segments[segments.count - 1]["end_ms"] = e.end * 1000
                    } else {
                        segments.append([
                            "start_ms": from, "end_ms": e.end * 1000, "source": i,
                            "source_in_ms": from - (range(cameras[i]).0 * 1000),
                        ])
                    }
                }
                guard let first = segments.first, let last = segments.last else { throw .invalidArgument }
                plan["start_ms"] = first["start_ms"]
                plan["end_ms"] = last["end_ms"]
                plan["segments"] = segments
                plan["copy_possible"] = (o["cut"] as? String ?? "keyframe") == "keyframe"
                plan["labels"] = (0..<cameras.count).map { ["source": $0, "name": "Camera \(cameras[$0].channel + 1)", "time": true] }
            } else {
                let canvas = o["canvas"] as? [String: Any]
                let width = canvas?["width"] as? Int ?? 1920
                let height = canvas?["height"] as? Int ?? 1080
                guard (1...16_384).contains(width), (1...16_384).contains(height) else { throw .invalidArgument }
                let layout =
                    (o["layout"] as? String).flatMap { ["2up", "4up"].contains($0) ? $0 : nil }
                    ?? (cameras.count == 1 ? "1up" : cameras.count == 2 ? "2up" : "4up")
                let (cols, rows) = layout == "4up" ? (2, 2) : layout == "1up" ? (1, 1) : (width >= height ? (2, 1) : (1, 2))
                let lo = cameras.map { range($0).0 }.min()! * 1000
                let hi = cameras.map { range($0).1 }.max()! * 1000
                plan["layout"] = layout
                plan["start_ms"] = lo
                plan["end_ms"] = hi
                plan["copy_possible"] = false
                plan["canvas"] = ["width": width, "height": height]
                plan["cells"] = cameras.prefix(cols * rows).enumerated().map { (i, c) -> [String: Any] in
                    let x0 = (i % cols) * width / cols, x1 = (i % cols + 1) * width / cols
                    let y0 = (i / cols) * height / rows, y1 = (i / cols + 1) * height / rows
                    return [
                        "rect": ["x": x0, "y": y0, "width": x1 - x0, "height": y1 - y0], "source": i,
                        "fit": o["fit"] as? String ?? "fit", "present": [[range(c).0 * 1000, range(c).1 * 1000]],
                    ]
                }
            }
            return plan
        }
        return Self.json(plan)
    }

    public func soundWatch(camera: String, enabled: Bool) throws(CoreStatus) {
        let c = try Self.camera(try Self.object(camera))
        let changed = try configuredWorld { w throws(CoreStatus) -> Bool in
            if enabled {
                if w.watched.contains(c) { return false }
                guard w.watched.count < 64 else { throw .limit }
                w.watched.append(c)
                return true
            }
            guard let i = w.watched.firstIndex(of: c) else { return false }
            w.watched.remove(at: i)
            return true
        }
        if changed {
            emit(event: Self.json([
                "type": "sound_watch", "camera": Self.cameraObject(c), "state": enabled ? "watching" : "stopped",
            ]))
        }
    }

    public func recordingsLoudness(query: String) throws(CoreStatus) -> String {
        let o = try Self.object(query)
        guard let start = (o["start"] as? NSNumber)?.int64Value, let end = (o["end"] as? NSNumber)?.int64Value,
            end >= start, (0...253_402_300_799).contains(start)
        else { throw .invalidArgument }
        let bucket = o["bucket_s"] as? Int ?? 60
        guard (1...3600).contains(bucket) else { throw .invalidArgument }
        let count = Int((end - start) / Int64(bucket)) + 1
        guard count <= 100_000 else { throw .limit }
        return try configuredWorld { w throws(CoreStatus) -> String in
            var cameras = w.watched
            if let list = o["cameras"] as? [Any] {
                cameras = []
                for item in list { cameras.append(try Self.camera(item)) }
            }
            let lanes = cameras.map { c -> [String: Any] in
                let live = w.watched.contains(c) && Self.allowed(w, c)
                let mine = w.analyzed.filter { $0.camera == c }
                // A bucket is heard when the camera listens or an analyzed
                // download reaches into it.
                let heard = (0..<count).map { i -> Bool in
                    let from = start + Int64(i * bucket), to = from + Int64(bucket) - 1
                    return live || mine.contains { $0.start <= to && $0.end >= from }
                }
                let peak: [Any] = heard.map { $0 ? -40 as Any : NSNull() }
                let rms: [Any] = heard.map { $0 ? -50 as Any : NSNull() }
                return [
                    "camera": Self.cameraObject(c), "bucket_s": bucket, "start": start,
                    "peak_db": peak, "rms_db": rms,
                ]
            }
            return Self.json(["revision": w.revision, "lanes": lanes])
        }
    }

    public func soundLabelsPut(json: String) throws(CoreStatus) {
        let o = try Self.object(json)
        let c = try Self.camera(o["camera"])
        guard let analyzer = o["analyzer"] as? String, !analyzer.isEmpty, analyzer != "pattern-v1",
            let start = (o["start"] as? NSNumber)?.int64Value, let end = (o["end"] as? NSNumber)?.int64Value,
            end >= start, let labels = o["labels"] as? [[String: Any]]
        else { throw .invalidArgument }
        guard end - start <= 31 * Self.day, labels.count <= 20_000 else { throw .limit }
        for l in labels {
            guard let kind = l["kind"] as? String, !kind.isEmpty,
                kind.allSatisfy({ $0.isASCII && ($0.isLowercase || $0.isNumber || $0 == "_") }),
                let s = (l["start"] as? NSNumber)?.int64Value, let e = (l["end"] as? NSNumber)?.int64Value, e >= s
            else { throw .invalidArgument }
        }
        var parsed: [(kind: String, start: Int64, end: Int64)] = []
        for l in labels {
            if let k = l["kind"] as? String, let s = (l["start"] as? NSNumber)?.int64Value,
                let e = (l["end"] as? NSNumber)?.int64Value
            {
                parsed.append((k, s, e))
            }
        }
        let revision = try configuredWorld { w throws(CoreStatus) -> (Int, Int) in
            try Self.requireKnownCamera(w, c)
            w.labels.removeAll { $0.camera == c && $0.analyzer == analyzer && $0.start >= start && $0.start <= end }
            w.labels += parsed.map { (c, $0.kind, $0.start, $0.end, analyzer) }
            w.revision += 1
            w.incidentsRevision += 1
            return (w.revision, w.incidentsRevision)
        }
        emit(event: Self.json([
            "type": "recordings_changed", "device": c.device, "revision": revision.0, "start": start, "end": end,
        ]))
        emit(event: Self.json(["type": "incidents_changed", "revision": revision.1]))
    }

    public func soundAnalyzeDay(json: String) throws(CoreStatus) -> String {
        let o = try Self.object(json)
        let c = try Self.camera(o["camera"])
        guard let day = (o["day"] as? NSNumber)?.int64Value, day >= 0 else { throw .invalidArgument }
        let request = try configuredWorld { w throws(CoreStatus) -> UInt64 in
            try Self.requireKnownCamera(w, c)
            guard w.analyses.count < 32 else { throw .limit }
            let id = w.nextAnalysis
            w.nextAnalysis += 1
            w.analyses.append((id, c, day))
            return id
        }
        emit(event: Self.json(Self.analysisEvent(request, c, day, "queued")))
        scheduleRecordingWork()
        return Self.json(["request": request, "state": "queued"])
    }

    public func soundAnalyzeCancel(_ request: UInt64) throws(CoreStatus) {
        let gone = try configuredWorld { w throws(CoreStatus) -> (camera: RecordingCamera, day: Int64) in
            guard let i = w.analyses.firstIndex(where: { $0.request == request }) else { throw .unknownID }
            let a = w.analyses.remove(at: i)
            return (a.camera, a.day)
        }
        emit(event: Self.json(Self.analysisEvent(request, gone.camera, gone.day, "canceled")))
    }

    private static func analysisEvent(
        _ request: UInt64, _ c: RecordingCamera, _ day: Int64, _ state: String
    ) -> [String: Any] {
        ["type": "sound_analyze", "request": request, "camera": cameraObject(c), "day": day, "state": state]
    }

    /// Finishes every queued sound analysis from the synthetic downloads, as
    /// the core does from finished ones: with no downloads folder it fails
    /// `no_source`, with no finished download of that camera reaching into the
    /// day `no_audio`; otherwise `running`, then `done`, with every second of
    /// the downloads inside the day heard (loudness) and one `smoke_alarm`
    /// label per download (the pattern detector's, replacing its earlier
    /// ones over that audio).
    func completeSoundAnalyses() {
        typealias Outcome = (request: UInt64, camera: RecordingCamera, day: Int64, failure: String?, seconds: Int, labels: Int)
        let pending = recordings.locked { w -> [Outcome] in
            let p = w.analyses
            w.analyses.removeAll()
            return p.map { a -> Outcome in
                guard w.downloadsDir != nil else { return (a.request, a.camera, a.day, "no_source", 0, 0) }
                let dayStart = DeviceTime.dayStart(a.day)
                var spans: [(start: Int64, end: Int64)] = []
                for d in w.downloads where d.state == "done" && d.cameras.contains(a.camera) {
                    let s = max(d.start, dayStart), e = min(d.end, dayStart + Self.day - 1)
                    if s <= e { spans.append((s, e)) }
                }
                guard !spans.isEmpty else { return (a.request, a.camera, a.day, "no_audio", 0, 0) }
                let mic = RecordingCamera(device: a.camera.device, channel: a.camera.channel, lens: 0)
                var seconds: Int64 = 0
                for s in spans {
                    seconds += s.end - s.start + 1
                    w.analyzed.removeAll { $0.camera == mic && $0.start == s.start }
                    w.analyzed.append((mic, s.start, s.end))
                    w.labels.removeAll { $0.camera == a.camera && $0.analyzer == "pattern-v1" && $0.start >= s.start && $0.start <= s.end }
                    w.labels.append((a.camera, "smoke_alarm", s.start, s.start + min(4, s.end - s.start), "pattern-v1"))
                }
                w.revision += 1
                w.incidentsRevision += 1
                return (a.request, a.camera, a.day, nil, Int(seconds), spans.count)
            }
        }
        for a in pending {
            if a.failure == nil { emit(event: Self.json(Self.analysisEvent(a.request, a.camera, a.day, "running"))) }
            var last = Self.analysisEvent(a.request, a.camera, a.day, a.failure == nil ? "done" : "failed")
            if let reason = a.failure {
                last["reason"] = reason
            } else {
                last["seconds"] = a.seconds
                last["labels"] = a.labels
            }
            emit(event: Self.json(last))
            if a.failure == nil {
                let revision = recordings.locked { $0.incidentsRevision }
                emit(event: Self.json(["type": "incidents_changed", "revision": revision]))
            }
        }
    }
}
