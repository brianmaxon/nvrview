import Foundation

// The recordings screens' pure pieces (docs/UX.md "Recordings"): device-local
// days, the answers of the recordings queries as Swift values, the kind
// filters (an "animal" filter that matches every animal kind), the edits as
// JSON, and the downloads and thumbnails boards. Nothing here calls the core
// or touches a window, so it is tested on its own.

// MARK: Device-local time

/// Device-local seconds (docs/FFI-RECORDINGS.md, "Time"): seconds since
/// 1970-01-01 00:00:00 of the device's wall clock, no zone. Shown by
/// formatting them as if they were UTC.
public enum DeviceTime {
    public static let day: Int64 = 86_400

    /// The first second of the day `s` falls in.
    public static func dayStart(_ s: Int64) -> Int64 { s - ((s % day) + day) % day }

    /// Today on a device whose wall clock is `utcOffset` seconds ahead of UTC.
    public static func today(now: Date, utcOffset: Int) -> Int64 {
        dayStart(Int64(now.timeIntervalSince1970.rounded(.down)) + Int64(utcOffset))
    }

    /// The device's "now" in device-local seconds.
    public static func now(_ now: Date, utcOffset: Int) -> Int64 {
        Int64(now.timeIntervalSince1970.rounded(.down)) + Int64(utcOffset)
    }

    private static let utc = TimeZone(secondsFromGMT: 0)!

    private static var calendar: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = utc
        return c
    }

    /// The first second of the month `s` falls in, and of the next month.
    public static func month(containing s: Int64) -> (start: Int64, next: Int64) {
        let c = calendar
        let date = Date(timeIntervalSince1970: TimeInterval(s))
        let start = c.date(from: c.dateComponents([.year, .month], from: date)) ?? date
        let next = c.date(byAdding: .month, value: 1, to: start) ?? start
        return (Int64(start.timeIntervalSince1970), Int64(next.timeIntervalSince1970))
    }

    /// The month before or after (`by` months) the one `s` falls in: its first day.
    public static func month(_ s: Int64, adding by: Int) -> Int64 {
        let c = calendar
        let start = Date(timeIntervalSince1970: TimeInterval(month(containing: s).start))
        return Int64((c.date(byAdding: .month, value: by, to: start) ?? start).timeIntervalSince1970)
    }

    /// The month's days as weeks of seven (nil before the first and after
    /// the last day), weeks starting on `firstWeekday` (1 Sunday ... 7 Saturday).
    public static func weeks(ofMonth s: Int64, firstWeekday: Int = 1) -> [[Int64?]] {
        let (start, next) = month(containing: s)
        let weekday = calendar.component(.weekday, from: Date(timeIntervalSince1970: TimeInterval(start)))
        let lead = ((weekday - firstWeekday) % 7 + 7) % 7
        var cells: [Int64?] = Array(repeating: nil, count: lead)
        var d = start
        while d < next {
            cells.append(d)
            d += day
        }
        while cells.count % 7 != 0 { cells.append(nil) }
        return stride(from: 0, to: cells.count, by: 7).map { Array(cells[$0..<$0 + 7]) }
    }

    /// The day of the month (1...31).
    public static func dayOfMonth(_ s: Int64) -> Int {
        calendar.component(.day, from: Date(timeIntervalSince1970: TimeInterval(s)))
    }

    /// Formats device-local seconds with a date format (`"HH:mm:ss"`, ...).
    public static func format(_ s: Int64, _ pattern: String) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = utc
        f.dateFormat = pattern
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(s)))
    }

    /// "Sunday, October 11, 2026" style, in the user's language.
    public static func longDay(_ s: Int64) -> String {
        let f = DateFormatter()
        f.timeZone = utc
        f.dateStyle = .full
        f.timeStyle = .none
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(s)))
    }

    /// "October 2026".
    public static func monthTitle(_ s: Int64) -> String {
        let f = DateFormatter()
        f.timeZone = utc
        f.setLocalizedDateFormatFromTemplate("MMMMyyyy")
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(s)))
    }

    /// A time of day, "07:42:05" (24 hours, as devices burn it into the picture).
    public static func time(_ s: Int64) -> String { format(s, "HH:mm:ss") }

    /// "07:42:05–07:42:40".
    public static func span(_ start: Int64, _ end: Int64) -> String { time(start) + "–" + time(end) }
}

// MARK: Kinds

/// The recordings screens' kind filters. Event kinds use the live detection
/// vocabulary (`IndicatorKind`), so the badges are the same; an "animal"
/// filter matches every animal kind a device reports, today's `dog_cat` and
/// any finer kind later (FFI-RECORDINGS.md, Decision 10).
public enum KindFilter: String, CaseIterable, Sendable, Hashable, Codable {
    case people, vehicle, animal, motion, other

    public var label: String {
        switch self {
        case .people: "People"
        case .vehicle: "Vehicles"
        case .animal: "Animals"
        case .motion: "Motion"
        case .other: "Other"
        }
    }

    public var symbolName: String {
        switch self {
        case .people: IndicatorKind.person.symbolName
        case .vehicle: IndicatorKind.vehicle.symbolName
        case .animal: IndicatorKind.animal.symbolName
        case .motion: IndicatorKind.motion.symbolName
        case .other: IndicatorKind.other.symbolName
        }
    }

    /// The filter an event kind belongs to.
    public init(kind: String) {
        switch IndicatorKind(detectionKind: kind) {
        case .person: self = .people
        case .vehicle: self = .vehicle
        case .animal: self = .animal
        case .motion: self = .motion
        default: self = .other
        }
    }

    public func matches(_ kind: String) -> Bool { KindFilter(kind: kind) == self }

    /// Whether any of `kinds` passes `filters` (an empty set passes all).
    public static func passes(_ kinds: [String], _ filters: Set<KindFilter>) -> Bool {
        filters.isEmpty || kinds.contains { filters.contains(KindFilter(kind: $0)) }
    }
}

extension IndicatorKind {
    /// Animal kinds: the device's `dog_cat`, a plain `animal`, and the finer
    /// names a device or a local classifier may use (`dog`, `cat`, `bird`,
    /// `pet`, `wildlife`, anything ending in `_animal`). Sound kinds such as
    /// `dog_bark` are sounds, not animals seen.
    public static func isAnimal(_ kind: String) -> Bool {
        ["dog_cat", "animal", "dog", "cat", "bird", "pet", "wildlife"].contains(kind) || kind.hasSuffix("_animal")
    }
}

// MARK: Answers

/// A camera's list in a calendar answer.
public struct CameraDays: Sendable, Equatable, Decodable {
    public var camera: RecordingCamera
    public var days: [Int64]
}

/// `nv_recordings_calendar`.
public struct CalendarAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var days: [CameraDays]

    /// Every day any camera has.
    public var allDays: Set<Int64> { Set(days.flatMap(\.days)) }
}

/// One recording file's range (a span of a timeline lane).
public struct RecordingSpan: Sendable, Equatable, Decodable {
    public var start: Int64
    public var end: Int64
    public var causes: [String]?
    public var tier: String?
}

public struct CameraSpans: Sendable, Equatable, Decodable {
    public var camera: RecordingCamera
    public var spans: [RecordingSpan]
}

/// `nv_recordings_spans`.
public struct SpansAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var truncated: Bool?
    public var spans: [CameraSpans]
}

/// An event of the index (device, local video, or local audio).
public struct RecordedEvent: Sendable, Equatable, Hashable, Decodable {
    public var camera: RecordingCamera
    public var kind: String
    public var source: String?
    public var start: Int64
    public var end: Int64
}

/// `nv_recordings_events`.
public struct EventsAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var truncated: Bool?
    public var events: [RecordedEvent]
}

/// One camera of an incident.
public struct IncidentCamera: Sendable, Equatable, Decodable {
    public var camera: RecordingCamera
    public var name: String?
    public var intervals: [[Int64]]
    public var download: [Int64]?
    public var added: Bool

    private enum CodingKeys: String, CodingKey { case camera, name, intervals, download, added }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(RecordingCamera.self, forKey: .camera)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        intervals = try c.decodeIfPresent([[Int64]].self, forKey: .intervals) ?? []
        download = try c.decodeIfPresent([Int64].self, forKey: .download)
        added = try c.decodeIfPresent(Bool.self, forKey: .added) ?? false
    }

    /// The range its cover thumbnail is asked for: its first interval, else
    /// the incident's range.
    public func coverRange(incidentStart: Int64, incidentEnd: Int64) -> (Int64, Int64) {
        if let first = intervals.first, first.count == 2 { return (first[0], first[1]) }
        return (incidentStart, incidentEnd)
    }
}

/// An incident (docs/FFI-RECORDINGS.md section 3). `key` is stable while it
/// grows and opaque: stored and passed back, never parsed.
public struct Incident: Sendable, Equatable, Identifiable, Decodable {
    public var key: String
    public var id: String
    public var name: String?
    public var group: String?
    public var start: Int64
    public var end: Int64
    public var kinds: [String]
    public var growing: Bool
    public var edited: Bool
    public var locked: Bool
    public var cameras: [IncidentCamera]
    public var events: [RecordedEvent]
    public var downloads: [String]

    private enum CodingKeys: String, CodingKey {
        case key, id, name, group, start, end, kinds, growing, edited, locked, cameras, events, downloads
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        key = try c.decode(String.self, forKey: .key)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? key
        name = try c.decodeIfPresent(String.self, forKey: .name)
        group = try c.decodeIfPresent(String.self, forKey: .group)
        start = try c.decode(Int64.self, forKey: .start)
        end = try c.decode(Int64.self, forKey: .end)
        kinds = try c.decodeIfPresent([String].self, forKey: .kinds) ?? []
        growing = try c.decodeIfPresent(Bool.self, forKey: .growing) ?? false
        edited = try c.decodeIfPresent(Bool.self, forKey: .edited) ?? false
        locked = try c.decodeIfPresent(Bool.self, forKey: .locked) ?? false
        cameras = try c.decodeIfPresent([IncidentCamera].self, forKey: .cameras) ?? []
        events = try c.decodeIfPresent([RecordedEvent].self, forKey: .events) ?? []
        downloads = try c.decodeIfPresent([String].self, forKey: .downloads) ?? []
    }

    /// The name to show: the user's, else the time and the kinds.
    public var title: String {
        if let name, !name.isEmpty { return name }
        let kinds = self.kinds.map { IndicatorKind(detectionKind: $0).label }
        var seen = Set<String>()
        let unique = kinds.filter { seen.insert($0).inserted }
        return DeviceTime.time(start) + " " + (unique.isEmpty ? "Recording" : unique.joined(separator: ", "))
    }

    /// Whether the incident involves `camera`.
    public func involves(_ camera: RecordingCamera) -> Bool { cameras.contains { $0.camera == camera } }

    /// The cover thumbnail key of each camera (sub-stream).
    public var coverKeys: [ThumbnailKey] {
        cameras.map { c in
            let (s, e) = c.coverRange(incidentStart: start, incidentEnd: end)
            return ThumbnailKey(camera: c.camera, tier: "sub", start: s, end: e)
        }
    }
}

/// `nv_incidents_get`.
public struct IncidentsAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var truncated: Bool?
    public var incidents: [Incident]
}

/// `nv_incident_get`.
public struct IncidentAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var incident: Incident
}

/// The answer of `nv_incident_lock` (download members only when locking).
public struct LockAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var keys: [String]
    public var download: String?
    public var folder: String?
    /// `queued`, `existing`, `when_settled`, `not_configured`, `limit`.
    public var downloadStatus: String?

    private enum CodingKeys: String, CodingKey {
        case revision, keys, download, folder
        case downloadStatus = "download_status"
    }

    /// What to tell the user after locking, if anything.
    public var message: String? {
        switch downloadStatus {
        case "queued": "Locked. Its video is downloading so it is kept."
        case "existing": "Locked. Its video is already downloaded or downloading."
        case "when_settled": "Locked. Its video downloads once the incident ends."
        case "not_configured": "Locked, but its video is not being kept: there is no downloads folder."
        case "limit": "Locked, but its video is not being kept: too many downloads. Forget finished ones."
        default: nil
        }
    }

    /// Whether the user must be told the video is not being kept.
    public var videoNotKept: Bool { downloadStatus == "not_configured" || downloadStatus == "limit" }
}

/// The answer of `nv_download_incident`.
public struct QueuedDownload: Sendable, Equatable, Decodable {
    public var download: String
    public var folder: String?
    public var existing: Bool?
}

/// The answer of `nv_download_trash`.
public struct TrashAnswer: Sendable, Equatable, Decodable {
    public var folder: String
}

/// `nv_recordings_status`.
public struct IndexStatus: Sendable, Equatable, Decodable {
    public struct Device: Sendable, Equatable, Decodable {
        public var device: String
        public var state: String
        public var pendingDays: Int?
        public var revision: Int?

        private enum CodingKeys: String, CodingKey {
            case device, state, revision
            case pendingDays = "pending_days"
        }
    }

    public var revision: Int
    public var devices: [Device]
}

/// Camera groups for incidents (`nv_camera_groups_get`/`put`): cameras that
/// cover the same area, so their events join one incident.
public struct IncidentGroups: Sendable, Equatable, Codable {
    public struct Group: Sendable, Equatable, Codable, Identifiable {
        public var name: String
        public var cameras: [RecordingCamera]
        public var id: String { name }

        public init(name: String, cameras: [RecordingCamera]) {
            self.name = name
            self.cameras = cameras
        }
    }

    public struct CameraName: Sendable, Equatable, Codable {
        public var camera: RecordingCamera
        public var name: String
    }

    public var revision: Int?
    public var groups: [Group]
    public var cameraNames: [CameraName]

    public init(groups: [Group] = [], cameraNames: [CameraName] = []) {
        self.groups = groups
        self.cameraNames = cameraNames
    }

    private enum CodingKeys: String, CodingKey {
        case revision, groups
        case cameraNames = "camera_names"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        revision = try c.decodeIfPresent(Int.self, forKey: .revision)
        groups = try c.decodeIfPresent([Group].self, forKey: .groups) ?? []
        cameraNames = try c.decodeIfPresent([CameraName].self, forKey: .cameraNames) ?? []
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(groups, forKey: .groups)
        try c.encode(cameraNames, forKey: .cameraNames)
    }

    /// The JSON `nv_camera_groups_put` takes (no revision). Empty names and
    /// empty groups are left out; a camera stays in its first group only.
    public var putJSON: String {
        var seen = Set<RecordingCamera>()
        var cleaned = IncidentGroups(cameraNames: cameraNames)
        for g in groups {
            let name = g.name.trimmingCharacters(in: .whitespaces)
            let cams = g.cameras.filter { seen.insert($0).inserted }
            if !name.isEmpty, !cams.isEmpty { cleaned.groups.append(Group(name: name, cameras: cams)) }
        }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return String(decoding: (try? encoder.encode(cleaned)) ?? Data("{}".utf8), as: UTF8.self)
    }
}

extension Decodable {
    /// Decodes an answer of the recordings calls (nil when it does not parse).
    public static func parse(answer json: String) -> Self? {
        try? JSONDecoder().decode(Self.self, from: Data(json.utf8))
    }
}

// MARK: Requests

/// Builds the JSON of the recordings requests.
public enum RecordingsRequest {
    static func string(_ s: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [s], options: [.withoutEscapingSlashes])) ?? Data(#"[""]"#.utf8)
        return String(decoding: data.dropFirst().dropLast(), as: UTF8.self)
    }

    static func cameras(_ cameras: [RecordingCamera]) -> String {
        "[" + cameras.map(\.json).joined(separator: ",") + "]"
    }

    /// `nv_recordings_enable`: these cameras of one device.
    public static func enable(_ cameras: [RecordingCamera], utcOffset: Int) -> String {
        let list = cameras.map { #"{"channel":\#($0.channel),"lens":\#($0.lens)}"# }.joined(separator: ",")
        return #"{"channels":[\#(list)],"utc_offset_s":\#(utcOffset)}"#
    }

    /// `nv_recordings_want`.
    public static func want(_ cameras: [RecordingCamera], days: [Int64], force: Bool = false) -> String {
        #"{"cameras":\#(Self.cameras(cameras)),"days":[\#(days.map(String.init).joined(separator: ","))],"force":\#(force)}"#
    }

    /// `nv_recordings_hold_today`.
    public static func hold(_ cameras: [RecordingCamera]) -> String { #"{"cameras":\#(Self.cameras(cameras))}"# }

    /// A query over `[start, end]`; `cameras` nil for all, `group` for a camera group.
    public static func query(
        start: Int64, end: Int64, cameras: [RecordingCamera]? = nil, group: String? = nil, limit: Int? = nil
    ) -> String {
        var parts = [#""start":\#(start)"#, #""end":\#(end)"#]
        if let cameras { parts.append(#""cameras":\#(Self.cameras(cameras))"#) }
        if let group { parts.append(#""group":\#(string(group))"#) }
        if let limit { parts.append(#""limit":\#(limit)"#) }
        return "{" + parts.joined(separator: ",") + "}"
    }

    /// `nv_download_incident` for the whole incident (every camera, main tier).
    public static func download(incident key: String) -> String { #"{"incident":\#(string(key))}"# }
}

/// An edit of incidents (`nv_incident_edit`).
public enum IncidentEdit: Sendable, Equatable {
    case merge([String])
    case split(key: String, at: Int64)
    case rename(key: String, name: String?)
    case addCamera(key: String, camera: RecordingCamera)
    case removeCamera(key: String, camera: RecordingCamera)
    case revert(key: String)

    public var json: String {
        let s = RecordingsRequest.string
        switch self {
        case .merge(let keys): return #"{"op":"merge","keys":[\#(keys.map(s).joined(separator: ","))]}"#
        case .split(let key, let at): return #"{"op":"split","key":\#(s(key)),"at":\#(at)}"#
        case .rename(let key, let name):
            let n = name.flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : s($0) } ?? "null"
            return #"{"op":"rename","key":\#(s(key)),"name":\#(n)}"#
        case .addCamera(let key, let camera): return #"{"op":"add_camera","key":\#(s(key)),"camera":\#(camera.json)}"#
        case .removeCamera(let key, let camera):
            return #"{"op":"remove_camera","key":\#(s(key)),"camera":\#(camera.json)}"#
        case .revert(let key): return #"{"op":"revert","key":\#(s(key))}"#
        }
    }
}

/// The answer of `nv_incident_edit`: the resulting incidents' keys.
public struct EditAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var keys: [String]
}

// MARK: Downloads

/// Every download the core reported, by ID, in the order they appeared.
/// Applies `download` and `download_removed` events; remembers which
/// downloads the user moved to the Trash, so the folder goes there once the
/// core says nothing writes into it.
public struct DownloadBoard: Sendable, Equatable {
    public private(set) var order: [String] = []
    public private(set) var byID: [String: DownloadProgress] = [:]
    /// Downloads handed to the Trash (`nv_download_trash`), with their folders.
    public private(set) var trashing: [String: String] = [:]

    public init() {}

    public var downloads: [DownloadProgress] { order.compactMap { byID[$0] } }

    /// Replaces everything with `nv_downloads_get`'s list.
    public mutating func load(_ list: [DownloadProgress]) {
        order = list.map(\.download)
        byID = Dictionary(list.map { ($0.download, $0) }, uniquingKeysWith: { _, b in b })
    }

    /// A `download` event: the newest state wins. Returns whether a state
    /// changed (the download's or a camera's, or a new download), as
    /// opposed to progress only.
    @discardableResult
    public mutating func apply(_ d: DownloadProgress) -> Bool {
        guard let old = byID[d.download] else {
            if trashing[d.download] != nil { return false }
            order.append(d.download)
            byID[d.download] = d
            return true
        }
        byID[d.download] = d
        return old.state != d.state || old.paused != d.paused || old.cameras.map(\.state) != d.cameras.map(\.state)
            || old.cameras.map(\.paused) != d.cameras.map(\.paused)
    }

    /// The user moved a download to the Trash; `folder` from the answer.
    public mutating func trashRequested(_ id: String, folder: String) {
        trashing[id] = folder
        remove(id)
    }

    /// A `download_removed` event. Returns the folder to move to the Trash
    /// now, if this was a trash and nothing writes there any more.
    public mutating func removed(_ id: String, trash: Bool, folder: String?) -> String? {
        remove(id)
        guard trash else { return nil }
        let asked = trashing.removeValue(forKey: id)
        return folder ?? asked
    }

    private mutating func remove(_ id: String) {
        byID[id] = nil
        order.removeAll { $0 == id }
    }
}

extension DownloadProgress {
    /// 0...1: bytes over the estimate when known, else chunks.
    public var fraction: Double {
        if state == "done" { return 1 }
        let chunks = cameras.reduce((0, 0)) { ($0.0 + $1.chunksDone, $0.1 + $1.chunksTotal) }
        if chunks.1 > 0 { return min(1, Double(chunks.0) / Double(chunks.1)) }
        return 0
    }

    /// The folder's last path component.
    public var folderName: String { folder.map { URL(fileURLWithPath: $0).lastPathComponent } ?? download }

    /// "Downloading", "Paused", "Failed: stalled", ...
    public var stateText: String {
        switch state {
        case "queued": return cameras.contains { $0.state == "waiting_device" } ? "Waiting for the device" : "Queued"
        case "running": return "Downloading"
        case "paused": return "Paused"
        case "finalizing": return "Finishing"
        case "done": return "Done"
        case "failed":
            let reasons = Set(cameras.compactMap(\.reason)).sorted()
            return "Failed" + (reasons.isEmpty ? "" : ": " + reasons.joined(separator: ", ").replacingOccurrences(of: "_", with: " "))
        default: return state
        }
    }

    /// Whether some job may still write (forget needs a pause first).
    public var isWorking: Bool { (state == "queued" || state == "running" || state == "finalizing") && !paused }

    /// The finished files.
    public var outputs: [String] { cameras.compactMap(\.output) }
}

// MARK: Thumbnails

/// What the screens know about one cover thumbnail.
public enum ThumbnailState: Sendable, Equatable {
    /// Asked; a `thumbnail` event follows.
    case pending
    /// The keyframe is being decoded into the small image.
    case decoding
    /// The app's small image (JPEG).
    case image(Data)
    /// No recording in the range.
    case empty
    /// The fetch failed, or the keyframe could not be decoded.
    case failed(String?)
    /// Dropped from the device's queue; asked again when shown again.
    case dropped
}

/// The thumbnails the screens hold, bounded: past `capacity`, the entries
/// used least recently go (they are asked again, from the core's cache, if
/// shown again).
public struct ThumbnailBoard: Sendable {
    public let capacity: Int
    private var states: [ThumbnailKey: ThumbnailState] = [:]
    private var used: [ThumbnailKey: UInt64] = [:]
    private var tick: UInt64 = 0

    public init(capacity: Int = 512) { self.capacity = max(capacity, 1) }

    public var count: Int { states.count }

    public subscript(key: ThumbnailKey) -> ThumbnailState? { states[key] }

    /// Whether a view showing `key` should ask the core for it now.
    public func needsRequest(_ key: ThumbnailKey) -> Bool {
        switch states[key] {
        case nil, .dropped: true
        default: false
        }
    }

    public mutating func set(_ key: ThumbnailKey, _ state: ThumbnailState) {
        tick += 1
        states[key] = state
        used[key] = tick
        guard states.count > capacity else { return }
        // Evict the least recently used entry that is not in flight.
        let victim = used.filter { k, _ in
            switch states[k] {
            case .pending, .decoding: false
            default: true
            }
        }.min { $0.value < $1.value }?.key
        if let victim {
            states[victim] = nil
            used[victim] = nil
        }
    }

    /// Marks `key` used (shown) without changing it.
    public mutating func touch(_ key: ThumbnailKey) {
        guard states[key] != nil else { return }
        tick += 1
        used[key] = tick
    }

    /// Every key waiting for the core (to forget them when the view moves on).
    public var pendingKeys: [ThumbnailKey] { states.filter { $0.value == .pending }.map(\.key) }

    public mutating func forgetPending() {
        for k in pendingKeys {
            states[k] = nil
            used[k] = nil
        }
    }
}
