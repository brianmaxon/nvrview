import Foundation

// Smart alerts (minor 12; docs/FFI.md "Smart alerts", docs/NOTIFICATIONS.md):
// the core takes in triggers, wakes cameras, and holds pictures; the app
// fetches each picture (`alertFramesGet`), decodes it, runs its detector,
// and answers with boxes (`alertDetectionsPut`). Decisions arrive as `alert`
// events and are kept in the event log (`alertsGet`).

/// A verification request's frames: Annex-B access units from a keyframe
/// (parameter sets included) to the picture to check.
public struct AlertFrames: Sendable, Equatable {
    /// `.h264Keyframe` or `.h265Keyframe` (the codec).
    public var kind: ThumbnailKind
    public var width: UInt32
    public var height: UInt32
    /// The picture's stream timestamp.
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

/// A camera angle as the alert JSON names it.
public struct AlertCamera: Sendable, Equatable, Hashable, Codable {
    public var device: UInt64
    public var channel: UInt16
    public var lens: UInt8

    public init(device: UInt64, channel: UInt16, lens: UInt8 = 0) {
        self.device = device
        self.channel = channel
        self.lens = lens
    }

    enum CodingKeys: String, CodingKey { case device, channel, lens }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        device = try c.decode(UInt64.self, forKey: .device)
        channel = try c.decode(UInt16.self, forKey: .channel)
        lens = try c.decodeIfPresent(UInt8.self, forKey: .lens) ?? 0
    }

    /// `{"device":..,"channel":..,"lens":..}`.
    public var json: String { #"{"device":\#(device),"channel":\#(channel),"lens":\#(lens)}"# }
}

/// A `verify_request` event: one picture to check.
public struct VerifyRequest: Sendable, Equatable, Decodable {
    public struct Frame: Sendable, Equatable, Decodable {
        public var ptsMicros: UInt64
        public var bytes: Int
        public var keyframe: Bool

        enum CodingKeys: String, CodingKey {
            case bytes, keyframe
            case ptsMicros = "pts_us"
        }
    }

    /// A region of the picture to run the detector on (normalized).
    public struct Crop: Sendable, Equatable, Decodable {
        /// `frame`, `tile`, or `region`.
        public var what: String
        public var x: Double
        public var y: Double
        public var w: Double
        public var h: Double
    }

    public var request: String
    /// `check` or `recheck` ("still present").
    public var purpose: String
    public var camera: AlertCamera
    public var triggerIDs: [UInt64]
    /// `h264` or `h265`.
    public var codec: String
    public var width: UInt32
    public var height: UInt32
    public var ptsMicros: UInt64
    public var bytes: Int
    public var frames: [Frame]
    public var fromCover: Bool
    public var atMs: Int64
    public var deadlineMs: Int64
    public var classes: [String]
    public var minScore: Double
    public var crops: [Crop]

    enum CodingKeys: String, CodingKey {
        case request, purpose, camera, codec, width, height, bytes, frames, classes, crops
        case triggerIDs = "trigger_ids"
        case ptsMicros = "pts_us"
        case fromCover = "from_cover"
        case atMs = "at_ms"
        case deadlineMs = "deadline_ms"
        case minScore = "min_score"
    }
}

/// One detector box for `alertDetectionsPut`, in normalized coordinates of
/// the whole picture.
public struct AlertDetection: Sendable, Equatable, Encodable {
    /// An alert class (`person`, `vehicle`, `animal`) or a detector label
    /// (`car`, `dog`); others are ignored by the core.
    public var label: String
    public var score: Double
    public var x: Double
    public var y: Double
    public var w: Double
    public var h: Double

    public init(label: String, score: Double, x: Double, y: Double, w: Double, h: Double) {
        self.label = label
        self.score = score
        self.x = x
        self.y = y
        self.w = w
        self.h = h
    }

    private struct Box: Encodable { var x, y, w, h: Double }
    enum CodingKeys: String, CodingKey { case label, score, box }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(label, forKey: .label)
        try c.encode(score, forKey: .score)
        try c.encode(Box(x: x, y: y, w: w, h: h), forKey: .box)
    }

    /// `{"detections":[...]}`.
    public static func json(_ list: [AlertDetection]) -> String {
        struct Wire: Encodable { var detections: [AlertDetection] }
        let data = (try? JSONEncoder().encode(Wire(detections: list))) ?? Data(#"{"detections":[]}"#.utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// The smart-alert events. Steps and decisions stay JSON (the trace and the
/// decision have many shapes; the views decode what they show).
public enum SmartAlertEvent: Sendable, Equatable {
    case verifyRequest(VerifyRequest)
    /// `answered`, `timeout`, or `canceled`.
    case verifyRequestDone(request: String, result: String)
    /// One trace step, as it happens (the live verification view).
    case verifyStep(triggerID: UInt64, json: String)
    /// A decision (`new`, `update`, `logged`) or an alert's end (`settled`);
    /// `json` is the whole event.
    case alert(state: String, json: String)
    /// `alerts_configured`: issues and the inputs' and outputs' results.
    case configured(json: String)
    case triggerRejected(reason: String)

    public static let types: Set<String> = [
        "verify_request", "verify_request_done", "verify_step", "alert", "alerts_configured",
        "trigger_rejected",
    ]

    private struct Head: Decodable {
        var type: String
        var request: String?
        var result: String?
        var state: String?
        var reason: String?
        var trigger_id: UInt64?
    }

    public static func parse(_ json: Data) -> SmartAlertEvent? {
        guard let h = try? JSONDecoder().decode(Head.self, from: json) else { return nil }
        let text = String(decoding: json, as: UTF8.self)
        switch h.type {
        case "verify_request":
            return (try? JSONDecoder().decode(VerifyRequest.self, from: json)).map(Self.verifyRequest)
        case "verify_request_done":
            guard let r = h.request, let result = h.result else { return nil }
            return .verifyRequestDone(request: r, result: result)
        case "verify_step":
            guard let t = h.trigger_id else { return nil }
            return .verifyStep(triggerID: t, json: text)
        case "alert":
            guard let s = h.state else { return nil }
            return .alert(state: s, json: text)
        case "alerts_configured":
            return .configured(json: text)
        case "trigger_rejected":
            return .triggerRejected(reason: h.reason ?? "")
        default:
            return nil
        }
    }
}

// Cores without smart alerts (minor < 12) refuse every call with
// `.unsupported`.
extension CoreAPI {
    public func alertsConfigure(json: String) throws(CoreStatus) { throw .unsupported }
    public func alertsGet(query: String?) throws(CoreStatus) -> String { throw .unsupported }
    public func alertsTriggerPut(json: String) throws(CoreStatus) -> UInt64 { throw .unsupported }
    public func alertFramesGet(request: String) throws(CoreStatus) -> AlertFrames { throw .unsupported }
    public func alertDetectionsPut(request: String, json: String) throws(CoreStatus) { throw .unsupported }
    public func alertSnapshotPut(alertID: UInt64, data: Data) throws(CoreStatus) { throw .unsupported }
    public func alertSnapshotGet(alertID: UInt64) throws(CoreStatus) -> Data { throw .unsupported }
    public func alertFeedback(json: String) throws(CoreStatus) { throw .unsupported }
    public func alertMute(cameraJSON: String, seconds: UInt32) throws(CoreStatus) { throw .unsupported }
}
