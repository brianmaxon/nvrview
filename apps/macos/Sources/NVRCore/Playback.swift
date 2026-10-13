import Foundation

// Recorded playback (ABI 3 minor 6; docs/FFI.md "Recorded playback (minor
// 6)", docs/FFI-RECORDINGS.md section 6) as Swift sees it: the value types
// of the calls, request builders, the parsed events, and defaults for cores
// without playback. A playback's tiles are tiles: their actions arrive at
// the sink like a live tile's and are answered the same way. `CCore`
// implements the calls over the C functions, `FakeCore` models them.

/// How a seek shows its target (`NvSeekMode`).
public enum SeekMode: UInt32, Sendable, Equatable {
    /// The keyframe at or before the position first (fast; scrubbing).
    case keyframe = 0
    /// Decode from that keyframe and show the picture at the position.
    case exact = 1
}

/// A playback's state (`NvPlaybackState`, and `state` in `playback_state`).
public enum PlaybackState: UInt32, Sendable, Equatable {
    /// Playing, but the clock waits for a camera.
    case buffering = 0
    case playing = 1
    case paused = 2
    /// At the end of the range or of the recordings.
    case ended = 3
    /// Every camera failed.
    case failed = 4

    /// The name the events use.
    public var name: String {
        switch self {
        case .buffering: "buffering"
        case .playing: "playing"
        case .paused: "paused"
        case .ended: "ended"
        case .failed: "failed"
        }
    }

    public init?(name: String) {
        switch name {
        case "buffering": self = .buffering
        case "playing": self = .playing
        case "paused": self = .paused
        case "ended": self = .ended
        case "failed": self = .failed
        default: return nil
        }
    }
}

/// What `playbackOpen` made: the playback and one tile per camera, in
/// request order.
public struct OpenedPlayback: Sendable, Equatable {
    public var playback: UInt64
    public var tiles: [UInt64]

    public init(playback: UInt64, tiles: [UInt64]) {
        self.playback = playback
        self.tiles = tiles
    }
}

/// The scrubber's reading (`nv_playback_position`).
public struct PlaybackPosition: Sendable, Equatable {
    /// Device-local milliseconds.
    public var positionMs: Int64
    public var state: PlaybackState

    public init(positionMs: Int64, state: PlaybackState) {
        self.positionMs = positionMs
        self.state = state
    }
}

/// One camera of a playback request (`{"camera":{...},"tier":"sub","offset_ms":0}`).
public struct PlaybackCamera: Sendable, Equatable {
    public var camera: RecordingCamera
    /// `sub` or `main`.
    public var tier: String
    public var offsetMs: Int64

    public init(camera: RecordingCamera, tier: String = "sub", offsetMs: Int64 = 0) {
        self.camera = camera
        self.tier = tier
        self.offsetMs = offsetMs
    }

    /// The JSON object `playbackOpen` (in `cameras`) and `playbackAddCamera` take.
    public var json: String {
        #"{"camera":\#(camera.json),"tier":"\#(tier)","offset_ms":\#(offsetMs)}"#
    }
}

/// Builders of `playbackOpen`'s JSON.
public enum PlaybackRequest {
    /// A range of these cameras (device-local milliseconds), opened paused at
    /// `positionMs` (the start when nil). `rate` 0.25, 0.5, 1, 2, 4, 8, or 16;
    /// `sync` `wait` or `free`.
    public static func range(
        startMs: Int64, endMs: Int64, positionMs: Int64? = nil, rate: Double = 1, sync: String = "wait",
        cameras: [PlaybackCamera]
    ) -> String {
        let position = positionMs.map { #","position_ms":\#($0)"# } ?? ""
        let list = cameras.map(\.json).joined(separator: ",")
        return #"{"start_ms":\#(startMs),"end_ms":\#(endMs)\#(position),"rate":\#(rate),"sync":"\#(sync)","cameras":[\#(list)]}"#
    }

    /// An incident's cameras and range with its margins.
    public static func incident(key: String, tier: String = "sub") -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [key])) ?? Data(#"[""]"#.utf8)
        let quoted = String(decoding: data.dropFirst().dropLast(), as: UTF8.self)
        return #"{"incident":\#(quoted),"tier":"\#(tier)"}"#
    }
}

/// The playback events of FFI-RECORDINGS.md sections 6 and 8. Unknown
/// fields are ignored; `parse` returns nil for other types and for events
/// missing what they cannot do without.
public enum PlaybackEvent: Sendable, Equatable {
    /// On every state change and after every seek; not periodic.
    case state(playback: UInt64, state: PlaybackState, positionMs: Int64, rate: Double)
    /// One camera: `buffering`, `playing`, `late`, `no_recording`, `ended`,
    /// `failed` (`reason`: `start_rejected`, `stream_ended`, `undecodable`,
    /// `session_closed`, `limit`, `device_in_use`), and with `start_rejected`
    /// the status the device refused the play with, when it answered one.
    case camera(playback: UInt64, tile: UInt64, state: String, reason: String?, status: Int? = nil)
    /// A seek's progress: `starting`, `active`, `failed`, with its generation.
    case seek(playback: UInt64, positionMs: Int64, generation: UInt32, state: String)
    /// `closed`, `device_removed`, `login_failed`, `core_shutdown`.
    case closed(playback: UInt64, reason: String)

    /// The event types this enum parses.
    public static let types: Set<String> = ["playback_state", "playback_camera", "playback_seek", "playback_closed"]

    private struct Wire: Decodable {
        var type: String
        var playback: UInt64
        var tile: UInt64?
        var state: String?
        var reason: String?
        var status: Int?
        var position_ms: Int64?
        var rate: Double?
        var generation: UInt32?
    }

    public static func parse(_ json: Data) -> PlaybackEvent? {
        guard let w = try? JSONDecoder().decode(Wire.self, from: json) else { return nil }
        switch w.type {
        case "playback_state":
            guard let state = w.state.flatMap(PlaybackState.init(name:)), let position = w.position_ms else {
                return nil
            }
            return .state(playback: w.playback, state: state, positionMs: position, rate: w.rate ?? 1)
        case "playback_camera":
            guard let tile = w.tile, let state = w.state else { return nil }
            return .camera(playback: w.playback, tile: tile, state: state, reason: w.reason, status: w.status)
        case "playback_seek":
            guard let position = w.position_ms, let generation = w.generation, let state = w.state else { return nil }
            return .seek(playback: w.playback, positionMs: position, generation: generation, state: state)
        case "playback_closed":
            return .closed(playback: w.playback, reason: w.reason ?? "closed")
        default:
            return nil
        }
    }
}

// Cores without playback (minor < 6) refuse every call with `.unsupported`.
extension CoreAPI {
    public func playbackOpen(json: String) throws(CoreStatus) -> OpenedPlayback { throw .unsupported }
    public func playbackAddCamera(_ playback: UInt64, json: String) throws(CoreStatus) -> UInt64 {
        throw .unsupported
    }
    public func playbackRemoveCamera(_ playback: UInt64, tile: UInt64) throws(CoreStatus) { throw .unsupported }
    public func playbackPlay(_ playback: UInt64) throws(CoreStatus) { throw .unsupported }
    public func playbackPause(_ playback: UInt64) throws(CoreStatus) { throw .unsupported }
    public func playbackSeek(_ playback: UInt64, positionMs: Int64, mode: SeekMode) throws(CoreStatus) {
        throw .unsupported
    }
    public func playbackSetRate(_ playback: UInt64, rate: Double) throws(CoreStatus) { throw .unsupported }
    public func playbackSetOffset(_ playback: UInt64, tile: UInt64, offsetMs: Int64) throws(CoreStatus) {
        throw .unsupported
    }
    public func playbackPosition(_ playback: UInt64) throws(CoreStatus) -> PlaybackPosition { throw .unsupported }
    public func playbackStatus(_ playback: UInt64) throws(CoreStatus) -> String { throw .unsupported }
    public func playbackClose(_ playback: UInt64) throws(CoreStatus) { throw .unsupported }
}
