import Foundation

// Recordings, part 1 (ABI 3 minor 5; docs/FFI.md "Recordings (minor 5)",
// docs/FFI-RECORDINGS.md sections 1 to 5 and 8) as Swift sees it: the value
// types of the calls, the parsed events, and defaults for cores without
// them. Queries and answers stay JSON text (the contract's shapes); only the
// events, which the app routes, are parsed here. `CCore` implements the
// calls over the C functions, `FakeCore` models them.

/// A camera in recordings JSON: `{"device": "<device key>", "channel": 3,
/// "lens": 0}` (lens 0 when left out).
public struct RecordingCamera: Sendable, Hashable, Codable {
    public var device: String
    public var channel: Int
    public var lens: Int

    public init(device: String, channel: Int, lens: Int = 0) {
        self.device = device
        self.channel = channel
        self.lens = lens
    }

    private enum CodingKeys: String, CodingKey { case device, channel, lens }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        device = try c.decode(String.self, forKey: .device)
        channel = try c.decode(Int.self, forKey: .channel)
        lens = try c.decodeIfPresent(Int.self, forKey: .lens) ?? 0
    }

    /// The JSON object, for building requests.
    public var json: String {
        let data = (try? JSONSerialization.data(withJSONObject: [device])) ?? Data(#"[""]"#.utf8)
        let quoted = String(decoding: data.dropFirst().dropLast(), as: UTF8.self)
        return #"{"device":\#(quoted),"channel":\#(channel),"lens":\#(lens)}"#
    }
}

/// What a cover thumbnail's bytes are (`NvThumbKind`).
public enum ThumbnailKind: UInt32, Sendable, Equatable {
    case h264Keyframe = 1
    case h265Keyframe = 2
    /// The app's own small image (for example JPEG).
    case image = 3
}

/// A cover thumbnail (`NvThumbnail` and its bytes).
public struct Thumbnail: Sendable, Equatable {
    public var kind: ThumbnailKind
    /// Of the picture the bytes decode to (0 when unknown).
    public var width: UInt32
    public var height: UInt32
    /// The keyframe's stream timestamp (0 for an image that did not keep it).
    public var ptsMicros: Int64
    public var data: Data

    public init(kind: ThumbnailKind, width: UInt32, height: UInt32, ptsMicros: Int64, data: Data) {
        self.kind = kind
        self.width = width
        self.height = height
        self.ptsMicros = ptsMicros
        self.data = data
    }
}

/// What `thumbnailGet` found.
public enum ThumbnailResult: Sendable, Equatable {
    case ready(Thumbnail)
    /// Not cached: a fetch is queued (or already was) and a `thumbnail`
    /// event follows (`NV_PENDING`).
    case pending
}

/// A thumbnail key (section 4): one camera, tier, and range.
public struct ThumbnailKey: Sendable, Hashable, Codable {
    public var camera: RecordingCamera
    public var tier: String
    public var start: Int64
    public var end: Int64

    public init(camera: RecordingCamera, tier: String = "sub", start: Int64, end: Int64) {
        self.camera = camera
        self.tier = tier
        self.start = start
        self.end = end
    }

    private enum CodingKeys: String, CodingKey { case camera, tier, start, end }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(RecordingCamera.self, forKey: .camera)
        tier = try c.decodeIfPresent(String.self, forKey: .tier) ?? "sub"
        start = try c.decode(Int64.self, forKey: .start)
        end = try c.decode(Int64.self, forKey: .end)
    }

    /// The JSON object the thumbnail functions take.
    public var json: String {
        #"{"camera":\#(camera.json),"tier":"\#(tier)","start":\#(start),"end":\#(end)}"#
    }
}

/// One camera of a download (the `download` event's `cameras`).
public struct DownloadCamera: Sendable, Equatable, Decodable {
    public var camera: RecordingCamera
    public var name: String?
    /// `waiting_device`, `queued`, `running`, `finalizing`, `done`, `failed`.
    public var state: String
    public var paused: Bool
    public var chunksDone: Int
    public var chunksTotal: Int
    public var bytes: Int64
    public var bytesEstimate: Int64?
    /// With `failed`: `stalled`, `session_closed`, `refused`, `no_recording`,
    /// `write_failed`, `join_failed`, `login_failed`, `other`.
    public var reason: String?
    /// English, for logs.
    public var detail: String?
    /// The finished MP4, once `done`.
    public var output: String?

    private enum CodingKeys: String, CodingKey {
        case camera, name, state, paused, chunks_done, chunks_total, bytes, bytes_estimate, reason, detail, output
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(RecordingCamera.self, forKey: .camera)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        state = try c.decodeIfPresent(String.self, forKey: .state) ?? "queued"
        paused = try c.decodeIfPresent(Bool.self, forKey: .paused) ?? false
        chunksDone = try c.decodeIfPresent(Int.self, forKey: .chunks_done) ?? 0
        chunksTotal = try c.decodeIfPresent(Int.self, forKey: .chunks_total) ?? 0
        bytes = try c.decodeIfPresent(Int64.self, forKey: .bytes) ?? 0
        bytesEstimate = try c.decodeIfPresent(Int64.self, forKey: .bytes_estimate)
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        detail = try c.decodeIfPresent(String.self, forKey: .detail)
        output = try c.decodeIfPresent(String.self, forKey: .output)
    }
}

/// A download's progress (the `download` event, and each entry of
/// `downloadsGet`).
public struct DownloadProgress: Sendable, Equatable, Decodable {
    public var download: String
    public var incident: String?
    public var folder: String?
    /// `queued`, `running`, `paused`, `finalizing`, `done`, `failed`.
    public var state: String
    public var paused: Bool
    public var cameras: [DownloadCamera]

    private enum CodingKeys: String, CodingKey { case download, incident, folder, state, paused, cameras }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        download = try c.decode(String.self, forKey: .download)
        incident = try c.decodeIfPresent(String.self, forKey: .incident)
        folder = try c.decodeIfPresent(String.self, forKey: .folder)
        state = try c.decode(String.self, forKey: .state)
        paused = try c.decodeIfPresent(Bool.self, forKey: .paused) ?? false
        cameras = try c.decodeIfPresent([DownloadCamera].self, forKey: .cameras) ?? []
    }
}

/// What a device's backend can do with recordings (`device_info`'s
/// `recordings`); all false for a core without recordings.
public struct RecordingSupport: Sendable, Equatable, Decodable {
    public var search: Bool
    public var playback: Bool
    public var download: Bool
    public var thumbnails: Bool

    public init(search: Bool = false, playback: Bool = false, download: Bool = false, thumbnails: Bool = false) {
        self.search = search
        self.playback = playback
        self.download = download
        self.thumbnails = thumbnails
    }

    private struct Info: Decodable { var recordings: RecordingSupport? }

    /// The flags of a `device_info` event (none when it has no `recordings`).
    public static func from(deviceInfo json: Data) -> RecordingSupport {
        (try? JSONDecoder().decode(Info.self, from: json))?.recordings ?? RecordingSupport()
    }

    private enum CodingKeys: String, CodingKey { case search, playback, download, thumbnails }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        search = try c.decodeIfPresent(Bool.self, forKey: .search) ?? false
        playback = try c.decodeIfPresent(Bool.self, forKey: .playback) ?? false
        download = try c.decodeIfPresent(Bool.self, forKey: .download) ?? false
        thumbnails = try c.decodeIfPresent(Bool.self, forKey: .thumbnails) ?? false
    }
}

/// The events of FFI-RECORDINGS.md section 8 (part 1). Unknown fields are
/// ignored; `parse` returns nil for other types and for events missing what
/// they cannot do without.
public enum RecordingsEvent: Sendable, Equatable {
    /// A device's search lane: `idle`, `busy`, `waiting_device`, `ended`.
    case index(device: String, state: String, pendingDays: Int, revision: Int)
    /// One camera-day could not be searched; not retried automatically.
    case dayFailed(device: String, camera: RecordingCamera?, day: Int64, reason: String?)
    /// Days `start` to `end` of a device changed; query again.
    case changed(device: String, revision: Int, start: Int64, end: Int64)
    case incidentsChanged(revision: Int)
    case cameraGroupsChanged(revision: Int)
    /// `ready` (call `thumbnailGet`), `empty`, `failed` (`reason`), `dropped`.
    case thumbnail(key: ThumbnailKey, state: String, reason: String?)
    case download(DownloadProgress)
    /// After forget or trash; with `trash`, the folder the app moves to the
    /// Trash now.
    case downloadRemoved(download: String, trash: Bool, folder: String?)

    /// The event types this enum parses.
    public static let types: Set<String> = [
        "recordings_index", "recordings_day_failed", "recordings_changed", "incidents_changed",
        "camera_groups_changed", "thumbnail", "download", "download_removed",
    ]

    private struct Wire: Decodable {
        var type: String
        var device: String?
        var state: String?
        var pending_days: Int?
        var revision: Int?
        var camera: RecordingCamera?
        var day: Int64?
        var reason: String?
        var start: Int64?
        var end: Int64?
        var key: ThumbnailKey?
        var download: String?
        var trash: Bool?
        var folder: String?
    }

    public static func parse(_ json: Data) -> RecordingsEvent? {
        guard let w = try? JSONDecoder().decode(Wire.self, from: json) else { return nil }
        switch w.type {
        case "recordings_index":
            guard let device = w.device, let state = w.state else { return nil }
            return .index(device: device, state: state, pendingDays: w.pending_days ?? 0, revision: w.revision ?? 0)
        case "recordings_day_failed":
            guard let device = w.device, let day = w.day else { return nil }
            return .dayFailed(device: device, camera: w.camera, day: day, reason: w.reason)
        case "recordings_changed":
            guard let device = w.device, let start = w.start, let end = w.end else { return nil }
            return .changed(device: device, revision: w.revision ?? 0, start: start, end: end)
        case "incidents_changed":
            guard let revision = w.revision else { return nil }
            return .incidentsChanged(revision: revision)
        case "camera_groups_changed":
            guard let revision = w.revision else { return nil }
            return .cameraGroupsChanged(revision: revision)
        case "thumbnail":
            guard let key = w.key, let state = w.state else { return nil }
            return .thumbnail(key: key, state: state, reason: w.reason)
        case "download":
            guard let d = try? JSONDecoder().decode(DownloadProgress.self, from: json) else { return nil }
            return .download(d)
        case "download_removed":
            guard let download = w.download else { return nil }
            return .downloadRemoved(download: download, trash: w.trash ?? false, folder: w.folder)
        default:
            return nil
        }
    }
}

/// The `downloads` list of `downloadsGet`.
public struct DownloadList: Sendable, Equatable, Decodable {
    public var downloads: [DownloadProgress]

    public static func parse(_ json: String) -> [DownloadProgress]? {
        (try? JSONDecoder().decode(DownloadList.self, from: Data(json.utf8)))?.downloads
    }
}

// Cores without recordings (minor < 5) refuse every call with
// `.unsupported`; the app hides recordings.
extension CoreAPI {
    public func recordingsConfigure(json: String) throws(CoreStatus) { throw .unsupported }
    public func recordingsEnable(deviceKey: String, json: String?) throws(CoreStatus) { throw .unsupported }
    public func recordingsCalendar(query: String) throws(CoreStatus) -> String { throw .unsupported }
    public func recordingsSpans(query: String) throws(CoreStatus) -> String { throw .unsupported }
    public func recordingsEvents(query: String) throws(CoreStatus) -> String { throw .unsupported }
    public func recordingsWant(json: String) throws(CoreStatus) { throw .unsupported }
    public func recordingsHoldToday(_ hold: UInt64, json: String?) throws(CoreStatus) { throw .unsupported }
    public func recordingsStatus() throws(CoreStatus) -> String { throw .unsupported }
    public func incidentsGet(query: String) throws(CoreStatus) -> String { throw .unsupported }
    public func incidentGet(key: String) throws(CoreStatus) -> String { throw .unsupported }
    public func incidentEdit(json: String) throws(CoreStatus) -> String { throw .unsupported }
    public func incidentLock(key: String, locked: Bool) throws(CoreStatus) -> String { throw .unsupported }
    public func cameraGroupsGet() throws(CoreStatus) -> String { throw .unsupported }
    public func cameraGroupsPut(json: String) throws(CoreStatus) { throw .unsupported }
    public func thumbnailGet(key: String) throws(CoreStatus) -> ThumbnailResult { throw .unsupported }
    public func thumbnailPut(key: String, thumbnail: Thumbnail) throws(CoreStatus) { throw .unsupported }
    public func thumbnailCancel(key: String?) throws(CoreStatus) { throw .unsupported }
    public func downloadIncident(json: String) throws(CoreStatus) -> String { throw .unsupported }
    public func downloadPause(_ download: String, camera: String?, paused: Bool) throws(CoreStatus) {
        throw .unsupported
    }
    public func downloadRetry(_ download: String, camera: String?) throws(CoreStatus) { throw .unsupported }
    public func downloadForget(_ download: String, deletePartial: Bool) throws(CoreStatus) { throw .unsupported }
    public func downloadTrash(_ download: String) throws(CoreStatus) -> String { throw .unsupported }
    public func downloadsGet() throws(CoreStatus) -> String { throw .unsupported }
    public func soundConfigure(json: String) throws(CoreStatus) { throw .unsupported }
}
