import Foundation

// The recordings' pure side in the app (docs/CAPTURE.md "Live recording",
// "Recording calls", "Indicators"): the settings as the core's configure
// JSON, the pills' text, the messages for a refused start and for a
// recording that ended by itself, and which cameras are being recorded,
// from the `recording` events. The app does the calls and draws.

/// "Record from": the stream a recording takes.
public enum RecordQuality: String, Sendable, CaseIterable, Equatable {
    case main, sub

    public var title: String { self == .main ? "Main stream" : "Sub-stream" }
}

/// The capture settings the core is configured with (`nv_capture_configure`).
public struct CaptureSettings: Sendable, Equatable {
    /// Bounds the settings allow (CAPTURE.md "Bounds").
    public static let minutesRange = 1...(12 * 60)
    public static let bytesRange: ClosedRange<UInt64> = 100_000_000...64_000_000_000

    /// "Save recordings to" (absolute).
    public var liveFolder: String
    /// "Record from".
    public var quality: RecordQuality = .main
    /// "Share-safe names" (shared with pictures).
    public var shareSafe = false
    /// "Stop a recording after" (minutes).
    public var maxMinutes = 60
    /// "… or at" (bytes, decimal).
    public var maxBytes: UInt64 = 4_000_000_000
    /// A recording does not start with less free than this.
    public var minFreeBytes: UInt64 = 2_000_000_000
    /// It stops when free space falls under this plus its own size.
    public var stopFreeBytes: UInt64 = 1_000_000_000
    public var maxLive = 4
    public var maxCalls = 2

    public init(liveFolder: String) { self.liveFolder = liveFolder }

    /// The settings within their bounds.
    public var clamped: CaptureSettings {
        var c = self
        c.maxMinutes = min(max(maxMinutes, Self.minutesRange.lowerBound), Self.minutesRange.upperBound)
        c.maxBytes = min(max(maxBytes, Self.bytesRange.lowerBound), Self.bytesRange.upperBound)
        return c
    }

    /// The `nv_capture_configure` JSON (docs/FFI.md "Capture").
    public var json: String {
        let c = clamped
        let o: [String: Any] = [
            "live_dir": c.liveFolder, "quality": c.quality.rawValue, "share_safe": c.shareSafe,
            "max_duration_s": c.maxMinutes * 60, "max_bytes": c.maxBytes, "min_free_bytes": c.minFreeBytes,
            "stop_free_bytes": c.stopFreeBytes, "max_live": c.maxLive, "max_calls": c.maxCalls,
        ]
        let data =
            (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// The words the recordings show.
public enum RecordingText {
    /// "0:42", "12:03", "1:02:03".
    public static func elapsed(ms: UInt64) -> String {
        let s = Int(ms / 1000)
        let (h, m, sec) = (s / 3600, s / 60 % 60, s % 60)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, sec) : String(format: "%d:%02d", m, sec)
    }

    /// The tile's pill: "● REC 1:23".
    public static func tilePill(ms: UInt64) -> String { "● REC \(elapsed(ms: ms))" }

    /// The call bar's pill: "● Recording 0:42".
    public static func callPill(ms: UInt64) -> String { "● Recording \(elapsed(ms: ms))" }

    /// "4 GB", "500 MB" (decimal, as the settings).
    public static func size(_ bytes: UInt64) -> String {
        if bytes >= 1_000_000_000 {
            let gb = Double(bytes) / 1e9
            return gb == gb.rounded() ? "\(Int(gb)) GB" : String(format: "%.1f GB", gb)
        }
        return "\(bytes / 1_000_000) MB"
    }

    /// "60 minutes", "1 minute", "2 hours", "90 minutes".
    public static func minutes(_ m: Int) -> String {
        if m >= 60, m % 60 == 0 { return m == 60 ? "60 minutes" : "\(m / 60) hours" }
        return m == 1 ? "1 minute" : "\(m) minutes"
    }

    /// What a refused start says, and whether to offer the sub-stream
    /// instead (a device already records two main streams).
    public static func refused(_ error: RecordStartError, settings: CaptureSettings, call: Bool)
        -> (text: String, offerSubStream: Bool)
    {
        switch (error.status, error.limit) {
        case (.limit, "live"?):
            return ("Already recording \(settings.maxLive) cameras, the most at a time. Stop one to record another.", false)
        case (.limit, "calls"?):
            return ("This call is already recorded, or \(settings.maxCalls) calls are, the most at a time.", false)
        case (.limit, "main_streams"?):
            return ("This device already records two main streams, the most it allows.", true)
        case (.limit, "disk_low"?):
            return ("Not enough free disk space to record: at least \(size(settings.minFreeBytes)) is needed.", false)
        case (.limit, _):
            return ("Recording is at a limit and cannot start.", false)
        case (.notConfigured, _):
            return (call ? "Call recording needs the audio encoder, which is not available." : "Recording is not set up.", false)
        case (.unknownID, _):
            return ("This camera's stream is not open, so it cannot be recorded.", false)
        case (.unsupported, _):
            return ("This version of the core cannot record.", false)
        default:
            return ("Recording could not start (\(error.status)).", false)
        }
    }

    /// What an ended recording says: nil for a quit (the app is going
    /// away). `finished` with `stopped` is "Recording saved".
    public static func ended(_ e: RecordingEvent, settings: CaptureSettings) -> String? {
        let what = e.kind == "call" ? "Call recording" : "Recording"
        if e.state == "recovered" { return "An unfinished recording was completed" }
        switch e.reason {
        case "stopped"?: return e.state == "failed" ? "\(what) failed" : "\(what) saved"
        case "core_shutdown"?: return nil
        case "max_duration"?: return "\(what) stopped at its \(minutes(settings.clamped.maxMinutes)) limit"
        case "max_size"?: return "\(what) stopped at its \(size(settings.clamped.maxBytes)) limit"
        case "disk_low"?: return "\(what) stopped: the disk is almost full"
        case "device_removed"?: return "\(what) stopped: the device was removed"
        case "login_failed"?: return "\(what) stopped: the device refused the login"
        case "write_failed"?: return "\(what) stopped: the file could not be written"
        case "encoder_failed"?: return "\(what) stopped: its sound could not be encoded"
        case "no_video"?: return "\(what) stopped: no picture arrived"
        case "format_changed"?: return "\(what) stopped: the stream changed"
        case "long_gap"?: return "\(what) stopped after a long outage"
        default: return e.state == "failed" ? "\(what) failed" : "\(what) saved"
        }
    }
}

/// The recordings that have not ended, by ID, with their camera: what the
/// pills, the Record buttons, the Camera menu's Recordings submenu and Stop
/// All read. Fed the app's own starts (`begin`, so the camera is known even
/// for a device that goes away) and the core's `recording` events.
public struct RecordingBoard: Sendable, Equatable {
    public struct Entry: Sendable, Equatable {
        public var id: UInt64
        /// `live` or `call`.
        public var kind: String
        public var camera: CameraRef?
        public var file: String
        public var state: String
        public var tier: String
        public var durationMs: UInt64
        /// The last event (nil until the first arrives).
        public var event: RecordingEvent?

        public var isCall: Bool { kind == "call" }
    }

    public private(set) var running: [UInt64: Entry] = [:]
    /// The latest ended recordings (newest last, at most `endedKept`).
    public private(set) var ended: [RecordingEvent] = []
    public static let endedKept = 32

    public init() {}

    /// The app started a recording.
    public mutating func begin(_ id: UInt64, kind: String, camera: CameraRef, file: String, tier: String) {
        running[id] = Entry(
            id: id, kind: kind, camera: camera, file: file, state: "waiting_keyframe", tier: tier, durationMs: 0)
    }

    /// A `recording` event; `camera` resolves the event's device ID for a
    /// recording the app did not start (one recovered after a crash).
    /// Returns the event when the recording ended (and was known or
    /// recovered), nil otherwise.
    @discardableResult
    public mutating func apply(_ e: RecordingEvent, camera: (RecordingEvent) -> CameraRef?) -> RecordingEvent? {
        if e.ended {
            let known = running.removeValue(forKey: e.recording) != nil
            guard known || e.state == "recovered" else { return nil }
            ended.append(e)
            if ended.count > Self.endedKept { ended.removeFirst(ended.count - Self.endedKept) }
            return e
        }
        var entry =
            running[e.recording]
            ?? Entry(id: e.recording, kind: e.kind, camera: camera(e), file: e.file, state: e.state, tier: e.tier, durationMs: 0)
        entry.state = e.state
        entry.file = e.file
        entry.tier = e.tier
        entry.durationMs = e.durationMs
        entry.event = e
        running[e.recording] = entry
        return nil
    }

    /// The running recordings, oldest first.
    public var list: [Entry] { running.values.sorted { $0.id < $1.id } }

    /// Recordings of `camera` (its device, channel and lens).
    public func recordings(of camera: CameraRef) -> [Entry] { list.filter { $0.camera == camera } }

    public func live(of camera: CameraRef) -> Entry? { recordings(of: camera).first { !$0.isCall } }
    public func call(of camera: CameraRef) -> Entry? { recordings(of: camera).first(where: \.isCall) }

    /// The tile's pill for `camera`: the longest running recording of it.
    public func pill(for camera: CameraRef) -> String? {
        let mine = recordings(of: camera)
        guard !mine.isEmpty else { return nil }
        return RecordingText.tilePill(ms: mine.map(\.durationMs).max() ?? 0)
    }

    /// A camera's device key changed (the core's stable key arrived).
    public mutating func rekey(_ old: String, to new: String) {
        for (id, e) in running where e.camera?.device == old { running[id]?.camera = e.camera?.rekeyed(to: new) }
    }
}
