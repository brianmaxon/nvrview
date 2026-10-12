import Foundation

// Capture (minor 9; docs/FFI.md "Capture", docs/CAPTURE.md): live
// recordings of a tile's camera and call recordings. The core writes the
// files; the app supplies an AAC encoder (`CaptureEncoder`) for audio the
// camera does not send as AAC and for a call's mixed tracks.

/// What an opened encoder reports (`NvAudioEncoder.open`).
public struct EncoderOpened: Sendable, Equatable {
    /// The AudioSpecificConfig (two bytes for AAC-LC; at most `NV_ASC_MAX`).
    public var audioSpecificConfig: [UInt8]
    /// The encoder's delay in frames (samples), written as an edit list.
    public var primingFrames: UInt32

    public init(audioSpecificConfig: [UInt8], primingFrames: UInt32) {
        self.audioSpecificConfig = audioSpecificConfig
        self.primingFrames = primingFrames
    }
}

/// The app's AAC encoder (`NvAudioEncoder`). `track` is the audio track's
/// index in the file (live: 0; call: 0 Conversation, 1 Visitor, 2 You). The
/// core calls these on its recorders' threads (never at once for one
/// recording); they must return quickly. Answer each encoded frame with
/// `CoreAPI.recordAudioEncoded`, in order, from inside `encode` or `close`
/// or from any other thread, until `close` returns. The core keeps the
/// encoder until it is freed.
public protocol CaptureEncoder: AnyObject, Sendable {
    /// Opens an AAC-LC encoder for one track; nil if it cannot.
    func open(recording: UInt64, track: UInt32, sampleRate: UInt32, channels: UInt32, bitRate: UInt32)
        -> EncoderOpened?
    /// One frame of 1024 mono samples, borrowed for the call.
    func encode(recording: UInt64, track: UInt32, pcm: UnsafeBufferPointer<Float>)
    /// Ends the track; with `flush`, the encoder's remaining output first.
    func close(recording: UInt64, track: UInt32, flush: Bool)
}

/// `nv_record_start`'s answer.
public struct RecordingStarted: Sendable, Equatable {
    public var recording: UInt64
    /// The file being written (absolute).
    public var file: String
    /// `waiting_keyframe`.
    public var state: String

    public init(recording: UInt64, file: String, state: String) {
        self.recording = recording
        self.file = file
        self.state = state
    }

    private struct Wire: Decodable {
        var recording: UInt64
        var file: String
        var state: String
    }

    public static func parse(_ json: String) -> RecordingStarted? {
        guard let w = try? JSONDecoder().decode(Wire.self, from: Data(json.utf8)) else { return nil }
        return RecordingStarted(recording: w.recording, file: w.file, state: w.state)
    }
}

/// Why `recordStart` failed: the status, and for `.limit` which limit
/// (`live`, `calls`, `main_streams`, `disk_low`).
public struct RecordStartError: Error, Sendable, Equatable {
    public var status: CoreStatus
    public var limit: String?

    public init(_ status: CoreStatus, limit: String? = nil) {
        self.status = status
        self.limit = limit
    }

    private struct Wire: Decodable { var limit: String }

    /// The error of a status and the answer that came with it.
    public init(_ status: CoreStatus, answer: String) {
        self.status = status
        limit =
            status == .limit
            ? (try? JSONDecoder().decode(Wire.self, from: Data(answer.utf8)))?.limit : nil
    }
}

/// A `nv_record_start` request.
public enum RecordRequest: Sendable, Equatable {
    /// A live recording of a tile's camera. `quality` (`main` or `sub`) and
    /// `shareSafe` override the settings; `name` is the camera name for the
    /// file; `utcOffsetSeconds` is the computer's local offset (the file
    /// name uses local time).
    case live(tile: UInt64, quality: String? = nil, name: String? = nil, shareSafe: Bool? = nil, utcOffsetSeconds: Int)
    /// A call recording, written to the absolute `.mp4` path the app reserved.
    case call(tile: UInt64, file: String)

    /// The computer's offset from UTC now, for `live`.
    public static var localUTCOffsetSeconds: Int { TimeZone.current.secondsFromGMT() }

    public var json: String {
        var o: [String: Any]
        switch self {
        case .live(let tile, let quality, let name, let shareSafe, let offset):
            o = ["kind": "live", "tile": tile, "utc_offset_s": offset]
            if let quality { o["quality"] = quality }
            if let name { o["name"] = name }
            if let shareSafe { o["share_safe"] = shareSafe }
        case .call(let tile, let file):
            o = ["kind": "call", "tile": tile, "file": file]
        }
        let data =
            (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// A `recording` event (docs/FFI.md "Capture"): sent at once on a change of
/// state and about once a second while recording.
public struct RecordingEvent: Sendable, Equatable, Decodable {
    public var recording: UInt64
    /// `live` or `call`.
    public var kind: String
    public var device: UInt64
    public var channel: UInt8
    public var lens: UInt8
    /// `main` or `sub`.
    public var tier: String
    /// `waiting_keyframe`, `recording`, `segment`, `finalizing`, `finished`,
    /// `failed`, `recovered`.
    public var state: String
    /// With `finished` or `failed`: `stopped`, `max_duration`, `max_size`,
    /// `disk_low`, `device_removed`, `login_failed`, `core_shutdown`,
    /// `write_failed`, `encoder_failed`, `no_video`, `format_changed`,
    /// `long_gap`.
    public var reason: String?
    public var file: String
    public var segment: Int
    public var bytes: UInt64
    public var durationMs: UInt64
    public var gaps: Int
    /// `passthrough`, `encoded`, `none`, `unsupported`.
    public var audio: String
    public var tracks: [String]
    public var encodeDroppedMs: UInt64
    public var droppedFrames: UInt64?
    /// On the last event: the fragmented file was published as the `.mp4`.
    public var fragmented: Bool?

    /// Whether the recording has ended (`finished`, `failed`, `recovered`).
    public var ended: Bool { state == "finished" || state == "failed" || state == "recovered" }

    enum CodingKeys: String, CodingKey {
        case recording, kind, device, channel, lens, tier, state, reason, file, segment, bytes, gaps, audio,
            tracks, fragmented
        case durationMs = "duration_ms"
        case encodeDroppedMs = "encode_dropped_ms"
        case droppedFrames = "dropped_frames"
    }

    public static func parse(_ json: Data) -> RecordingEvent? {
        try? JSONDecoder().decode(RecordingEvent.self, from: json)
    }
}

// Cores without capture (minor < 9) refuse every call with `.unsupported`.
extension CoreAPI {
    public func captureConfigure(json: String) throws(CoreStatus) { throw .unsupported }
    public func captureSetEncoder(_ encoder: (any CaptureEncoder)?) throws(CoreStatus) { throw .unsupported }
    public func recordAudioEncoded(recording: UInt64, track: UInt32, unit: UnsafeRawBufferPointer) -> CoreStatus {
        .unsupported
    }
    public func recordStart(_ request: RecordRequest) throws(RecordStartError) -> RecordingStarted {
        throw RecordStartError(.unsupported)
    }
    public func recordStop(_ recording: UInt64) throws(CoreStatus) { throw .unsupported }
    public func recordStatus() throws(CoreStatus) -> String { throw .unsupported }
}
