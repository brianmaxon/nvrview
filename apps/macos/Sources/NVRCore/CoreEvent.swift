import Foundation

/// Where a device is in its connection life cycle (`device_state` events).
public enum DeviceState: Sendable, Equatable {
    case connecting
    case connected
    /// Waiting before the next attempt; `retryMilliseconds` from now.
    case backoff(attempt: Int, retryMilliseconds: Int)
    /// Terminal: the core does not retry (accounts lock out). `remainTimes`
    /// is how many further failures the device tolerates, `unlockSeconds`
    /// how long a lockout lasts, and `reason` the status in words, when known.
    case loginFailed(status: Int?, remainTimes: Int?, reason: String? = nil, unlockSeconds: Int? = nil)
    case closed
    /// A state this version does not know.
    case other(String)
}

public struct ChannelInfo: Sendable, Equatable, Codable {
    public var channel: Int
    public var connected: Bool
    public var tiers: [String]
    public var name: String?
    public var model: String?
    /// What the camera itself detects (ABI 3: the `ai` bits of the channel's
    /// capabilities, in the device's names: `people`, `vehicle`, `dog_cat`,
    /// ...). Empty when the camera has no detection of its own.
    public var detects: [String]
    /// How many lenses the camera has (`lenses`; absent means 1). A second
    /// lens is served on the same channel as its own tiers.
    public var lenses: Int

    private enum CodingKeys: String, CodingKey { case channel, connected, tiers, name, model, detects, lenses }

    public init(
        channel: Int, connected: Bool = false, tiers: [String] = [], name: String? = nil,
        model: String? = nil, detects: [String] = [], lenses: Int = 1
    ) {
        self.lenses = lenses
        self.channel = channel
        self.connected = connected
        self.tiers = tiers
        self.name = name
        self.model = model
        self.detects = detects
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        channel = try c.decode(Int.self, forKey: .channel)
        connected = try c.decodeIfPresent(Bool.self, forKey: .connected) ?? false
        tiers = try c.decodeIfPresent([String].self, forKey: .tiers) ?? []
        name = try c.decodeIfPresent(String.self, forKey: .name)
        model = try c.decodeIfPresent(String.self, forKey: .model)
        detects = (try? c.decodeIfPresent([String].self, forKey: .detects)) ?? []
        lenses = max((try? c.decodeIfPresent(Int.self, forKey: .lenses)) ?? 1, 1)
    }

    /// Whether the camera has a second lens the app can show.
    public var isMultiLens: Bool { lenses >= 2 }
}

public struct TileStats: Sendable, Equatable {
    public var received = 0
    public var decoded = 0
    public var gated = 0
    public var dropped = 0
    public var failures = 0
    public var resets = 0

    public init(received: Int = 0, decoded: Int = 0, gated: Int = 0, dropped: Int = 0, failures: Int = 0, resets: Int = 0) {
        self.received = received
        self.decoded = decoded
        self.gated = gated
        self.dropped = dropped
        self.failures = failures
        self.resets = resets
    }
}

/// A parsed `on_event` JSON object. Unknown `type` values and unknown fields
/// are ignored (docs/FFI.md), so `parse` returns nil for the former and the
/// decoder skips the latter.
public enum CoreEvent: Sendable, Equatable {
    case deviceState(device: UInt64, state: DeviceState)
    case deviceInfo(device: UInt64, name: String?, model: String?)
    case channels(device: UInt64, channels: [ChannelInfo])
    case tileStats(tile: UInt64, stats: TileStats)
    case tileClosed(tile: UInt64, reason: String?)
    case audioStats(tile: UInt64, stats: AudioStatsEvent)
    /// ABI 3: progress of `nv_tile_set_tier` (docs/FFI-ABI3.md section 1).
    /// `reason` comes with `failed` (`superseded`, `timeout`,
    /// `start_rejected`, `stream_ended`, `undecodable`, `session_closed`).
    case tileTier(tile: UInt64, tier: StreamTier?, state: TierSwitchState, generation: UInt32, reason: String? = nil)
    /// ABI 3: a detection started or stopped on a channel (section 2).
    case detection(DetectionEvent)
    /// ABI 3: the stored layouts changed; read them again. `revision` grows
    /// with every change.
    case layoutsChanged(revision: Int? = nil)
    /// ABI 3: a timing hook for the budgets (section 5), nanoseconds.
    case timing(what: String, tile: UInt64?, nanos: UInt64)

    private struct Wire: Decodable {
        var type: String
        var device: UInt64?
        var tile: UInt64?
        var state: String?
        var attempt: Int?
        var retry_ms: Int?
        var status: Int?
        var remain_times: Int?
        var unlock_s: Int?
        var name: String?
        var model: String?
        /// The `channels` event's list; other events use the key for a
        /// number (`audio_stats`: audio channels), which reads as nil.
        var channels: ChannelList?
        var received: Int?
        var decoded: Int?
        var gated: Int?
        var dropped: Int?
        var failures: Int?
        var resets: Int?
        var reason: String?
        var records: Int?
        var audible: Bool?
        var tapped: Bool?
        var codec: String?
        var playout_delay_ms: Double?
        var playout_delay_target_ms: Double?
        var av_offset_ms: Double?
        var underruns: Int?
        var resyncs: Int?
        var tap_dropped: Int?
        var channel: Int?
        var tier: String?
        var generation: UInt32?
        var kind: String?
        var source: String?
        var at_ns: UInt64?
        var what: String?
        var ns: UInt64?
        var lens: Int?
        var revision: Int?
    }

    struct ChannelList: Decodable {
        var list: [ChannelInfo]?
        init(from decoder: any Decoder) throws {
            list = try? decoder.singleValueContainer().decode([ChannelInfo].self)
        }
    }

    /// Nil for malformed JSON, an unknown type, or a known type missing the
    /// fields it cannot do without.
    public static func parse(_ json: Data) -> CoreEvent? {
        guard let w = try? JSONDecoder().decode(Wire.self, from: json) else { return nil }
        switch w.type {
        case "device_state":
            guard let device = w.device, let state = w.state else { return nil }
            let s: DeviceState
            switch state {
            case "connecting": s = .connecting
            case "connected": s = .connected
            case "backoff": s = .backoff(attempt: w.attempt ?? 0, retryMilliseconds: w.retry_ms ?? 0)
            case "login_failed":
                s = .loginFailed(
                    status: w.status, remainTimes: w.remain_times, reason: w.reason, unlockSeconds: w.unlock_s)
            case "closed": s = .closed
            default: s = .other(state)
            }
            return .deviceState(device: device, state: s)
        case "device_info":
            guard let device = w.device else { return nil }
            return .deviceInfo(device: device, name: w.name, model: w.model)
        case "channels":
            guard let device = w.device else { return nil }
            return .channels(device: device, channels: w.channels?.list ?? [])
        case "tile_stats":
            guard let tile = w.tile else { return nil }
            return .tileStats(
                tile: tile,
                stats: TileStats(
                    received: w.received ?? 0, decoded: w.decoded ?? 0, gated: w.gated ?? 0,
                    dropped: w.dropped ?? 0, failures: w.failures ?? 0, resets: w.resets ?? 0))
        case "tile_closed":
            guard let tile = w.tile else { return nil }
            return .tileClosed(tile: tile, reason: w.reason)
        case "audio_stats":
            guard let tile = w.tile else { return nil }
            var a = AudioStatsEvent()
            a.records = w.records ?? 0
            a.decoded = w.decoded ?? 0
            a.audible = w.audible ?? false
            a.tapped = w.tapped ?? false
            a.codec = w.codec
            a.playoutDelayMilliseconds = w.playout_delay_ms
            a.playoutDelayTargetMilliseconds = w.playout_delay_target_ms
            a.avOffsetMilliseconds = w.av_offset_ms
            a.underruns = w.underruns ?? 0
            a.resyncs = w.resyncs ?? 0
            a.tapDropped = w.tap_dropped ?? 0
            return .audioStats(tile: tile, stats: a)
        case "tile_tier":
            guard let tile = w.tile, let state = w.state, let tier = w.tier else { return nil }
            return .tileTier(
                tile: tile, tier: StreamTier(name: tier), state: TierSwitchState(state),
                generation: w.generation ?? 0, reason: w.reason)
        case "detection":
            guard let device = w.device, let channel = w.channel, let kind = w.kind, let state = w.state else {
                return nil
            }
            return .detection(
                DetectionEvent(
                    device: device, channel: channel, kind: kind.lowercased(), active: state == "start",
                    source: DetectionSource(w.source ?? "device"), atNanos: w.at_ns, lens: w.lens))
        case "alarm":
            // Other device states, such as a doorbell press (`visitor`).
            guard let device = w.device, let channel = w.channel, let kind = w.kind, let state = w.state else {
                return nil
            }
            return .detection(
                DetectionEvent(
                    device: device, channel: channel, kind: kind.lowercased(), active: state == "start",
                    source: DetectionSource(w.source ?? "device"), atNanos: w.at_ns, lens: w.lens, alarm: true))
        case "motion":
            guard let device = w.device, let channel = w.channel, let state = w.state else { return nil }
            return .detection(
                DetectionEvent(
                    device: device, channel: channel, kind: DetectionEvent.motionKind, active: state == "start",
                    source: .device, atNanos: w.at_ns, lens: w.lens))
        case "layouts_changed":
            return .layoutsChanged(revision: w.revision)
        case "timing":
            guard let what = w.what, let ns = w.ns else { return nil }
            return .timing(what: what, tile: w.tile, nanos: ns)
        default:
            return nil
        }
    }
}

/// Where a tile's stream-tier switch stands (`tile_tier` events).
public enum TierSwitchState: Sendable, Equatable {
    /// The new tier's stream and pipeline are starting; the old one shows.
    case starting
    /// The new generation's first picture decoded; it is the one shown now.
    case active
    /// The new tier could not start; the old one keeps showing.
    case failed
    case other(String)

    public init(_ s: String) {
        switch s {
        case "starting": self = .starting
        case "active": self = .active
        case "failed": self = .failed
        default: self = .other(s)
        }
    }
}

/// Who detected something: the camera's own AI, or a model on this Mac.
public enum DetectionSource: Sendable, Equatable, Hashable {
    case device
    case local
    case other(String)

    public init(_ s: String) {
        switch s {
        case "device": self = .device
        case "local": self = .local
        default: self = .other(s)
        }
    }
}

/// One `detection` (or `motion`) event: a kind of object started or stopped
/// being detected on a channel. `motion` events use the kind `motion`.
public struct DetectionEvent: Sendable, Equatable {
    public static let motionKind = "motion"

    public var device: UInt64
    public var channel: Int
    /// The device's name for it (`people`, `vehicle`, `dog_cat`, `face`,
    /// `package`, ...), passed through when unknown.
    public var kind: String
    public var active: Bool
    public var source: DetectionSource
    /// The core clock at arrival; the pushes carry no usable time.
    public var atNanos: UInt64?
    /// 1 for the second lens of a multi-lens camera, nil otherwise.
    public var lens: Int?
    /// An `alarm` event (a device state that is not an object or motion,
    /// such as a doorbell press).
    public var alarm: Bool

    public init(
        device: UInt64, channel: Int, kind: String, active: Bool, source: DetectionSource = .device,
        atNanos: UInt64? = nil, lens: Int? = nil, alarm: Bool = false
    ) {
        self.device = device
        self.channel = channel
        self.kind = kind
        self.active = active
        self.source = source
        self.atNanos = atNanos
        self.lens = lens
        self.alarm = alarm
    }
}
