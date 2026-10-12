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
    /// A fisheye (360°) camera sending its raw circle (`fisheye`; absent
    /// means false): the app can dewarp it (docs/RENDERING.md).
    public var fisheye: Bool
    /// The channel's item has talk-back (`talk`, `ipcAudioTalk` 1; absent
    /// means false until the capabilities are read).
    public var talk: Bool
    /// A doorbell (`doorbell`, `channelType` Doorbell).
    public var doorbell: Bool
    /// What the channel's item can move (`ptz`).
    public var ptz: PTZCapabilities
    /// Per lens of a multi-lens camera, lens 0 first (`lens_talk`,
    /// `lens_ptz`); nil when the core sent none.
    public var lensTalk: [Bool]?
    public var lensPTZ: [PTZCapabilities]?

    private enum CodingKeys: String, CodingKey {
        case channel, connected, tiers, name, model, detects, lenses, fisheye, talk, doorbell, ptz
        case lensTalk = "lens_talk"
        case lensPTZ = "lens_ptz"
    }

    public init(
        channel: Int, connected: Bool = false, tiers: [String] = [], name: String? = nil,
        model: String? = nil, detects: [String] = [], lenses: Int = 1, fisheye: Bool = false,
        talk: Bool = false, doorbell: Bool = false, ptz: PTZCapabilities = .none,
        lensTalk: [Bool]? = nil, lensPTZ: [PTZCapabilities]? = nil
    ) {
        self.talk = talk
        self.doorbell = doorbell
        self.ptz = ptz
        self.lensTalk = lensTalk
        self.lensPTZ = lensPTZ
        self.fisheye = fisheye
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
        fisheye = (try? c.decodeIfPresent(Bool.self, forKey: .fisheye)) ?? false
        talk = (try? c.decodeIfPresent(Bool.self, forKey: .talk)) ?? false
        doorbell = (try? c.decodeIfPresent(Bool.self, forKey: .doorbell)) ?? false
        ptz = (try? c.decodeIfPresent(PTZCapabilities.self, forKey: .ptz)) ?? .none
        lensTalk = try? c.decodeIfPresent([Bool].self, forKey: .lensTalk)
        lensPTZ = try? c.decodeIfPresent([PTZCapabilities].self, forKey: .lensPTZ)
    }

    /// Whether the camera has a second lens the app can show.
    public var isMultiLens: Bool { lenses >= 2 }

    /// Talk-back of one lens: its own entry when the core listed lenses,
    /// else the channel's item (docs/FFI.md, "channels").
    public func talk(lens: Int) -> Bool {
        if let lensTalk, lens < lensTalk.count { return lensTalk[lens] }
        return lens == 0 ? talk : false
    }

    /// What one lens can move, by the same rule.
    public func ptz(lens: Int) -> PTZCapabilities {
        if let lensPTZ, lens < lensPTZ.count { return lensPTZ[lens] }
        return lens == 0 ? ptz : .none
    }
}

/// A camera's motion capabilities (`ptz` in the `channels` event): what it
/// has, not what the user allowed.
public struct PTZCapabilities: Sendable, Equatable, Codable {
    /// `ptzType` 3.
    public var panTilt: Bool
    /// `ptzPreset` bit 0.
    public var presets: Bool
    /// `ptzType` 1 or 3.
    public var zoomFocus: Bool

    public static let none = PTZCapabilities()

    private enum CodingKeys: String, CodingKey {
        case panTilt = "pan_tilt"
        case presets
        case zoomFocus = "zoom_focus"
    }

    public init(panTilt: Bool = false, presets: Bool = false, zoomFocus: Bool = false) {
        self.panTilt = panTilt
        self.presets = presets
        self.zoomFocus = zoomFocus
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        panTilt = (try? c.decodeIfPresent(Bool.self, forKey: .panTilt)) ?? false
        presets = (try? c.decodeIfPresent(Bool.self, forKey: .presets)) ?? false
        zoomFocus = (try? c.decodeIfPresent(Bool.self, forKey: .zoomFocus)) ?? false
    }
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
    /// The tile's stream could not start (`tile_error`, reason such as
    /// `start_rejected`, with the device's status when it gave one).
    case tileError(tile: UInt64, reason: String?, status: Int?)
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
    case timing(what: String, tile: UInt64?, nanos: UInt64, tierSwitch: TierSwitchTiming? = nil)
    /// Talk-back progress (docs/FFI.md "Talk-back").
    case talk(TalkEvent)
    /// Minor 1: a search was started (docs/FFI.md, "Device discovery").
    case discoveryStarted(search: UInt64)
    /// Minor 1: a device answered, or answered again with more.
    case discovered(DiscoveredDevice)
    /// Minor 1: a search ended.
    case discoveryDone(DiscoveryEnd)
    /// Minor 2: an update check or download progressed or ended.
    case update(UpdateEvent)
    /// Minor 5: recordings (index, incidents, groups, thumbnails, downloads).
    case recordings(RecordingsEvent)
    /// Minor 6: recorded playback (state, cameras, seeks, closed).
    case playback(PlaybackEvent)
    /// Minor 9: a live or call recording's progress.
    case recording(RecordingEvent)

    /// The `discovered` event's own shape (its `channels` is a number, unlike
    /// the `channels` event's list).
    private struct DiscoveredWire: Decodable {
        struct Service: Decodable {
            var kind: String
            var port: UInt16?
            var url: String?
        }
        var search: UInt64
        var key: String
        var kind: String?
        var name: String?
        var model: String?
        var address: String
        var port: UInt16?
        var channels: Int?
        var battery_percent: Int?
        var services: [Service]?
    }

    private struct DoneWire: Decodable {
        var search: UInt64
        var count: Int?
        var stopped: Bool?
        var errors: [String]?
    }

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
        var delta_ms: Double?
        var held_ms: Double?
        var capped: Bool?
        var generation: UInt32?
        var kind: String?
        var source: String?
        var at_ns: UInt64?
        var what: String?
        var ns: UInt64?
        var lens: Int?
        var revision: Int?
        var sent_ms: Int?
        var dropped_ms: Int?
    }

    struct ChannelList: Decodable {
        var list: [ChannelInfo]?
        init(from decoder: any Decoder) throws {
            list = try? decoder.singleValueContainer().decode([ChannelInfo].self)
        }
    }

    /// Nil for malformed JSON, an unknown type, or a known type missing the
    /// fields it cannot do without.
    private struct TypeWire: Decodable { var type: String }

    public static func parse(_ json: Data) -> CoreEvent? {
        // Recordings events name devices by key (a string), not by ID.
        if let t = try? JSONDecoder().decode(TypeWire.self, from: json) {
            if RecordingsEvent.types.contains(t.type) {
                return RecordingsEvent.parse(json).map(CoreEvent.recordings)
            }
            if PlaybackEvent.types.contains(t.type) {
                return PlaybackEvent.parse(json).map(CoreEvent.playback)
            }
            if t.type == "recording" {
                return RecordingEvent.parse(json).map(CoreEvent.recording)
            }
        }
        guard let w = try? JSONDecoder().decode(Wire.self, from: json) else { return nil }
        switch w.type {
        case "update":
            guard let u = try? JSONDecoder().decode(UpdateEvent.self, from: json) else { return nil }
            return .update(u)
        case "discovery_started":
            guard let search = (try? JSONDecoder().decode(DoneWire.self, from: json))?.search else { return nil }
            return .discoveryStarted(search: search)
        case "discovered":
            guard let d = try? JSONDecoder().decode(DiscoveredWire.self, from: json) else { return nil }
            return .discovered(
                DiscoveredDevice(
                    search: d.search, key: d.key, kind: d.kind.flatMap(DiscoveredDevice.Kind.init(rawValue:)) ?? .unknown,
                    name: d.name, model: d.model, address: d.address, port: d.port, channels: d.channels,
                    batteryPercent: d.battery_percent,
                    services: (d.services ?? []).map { DiscoveredService(kind: $0.kind, port: $0.port, url: $0.url) }))
        case "discovery_done":
            guard let d = try? JSONDecoder().decode(DoneWire.self, from: json) else { return nil }
            return .discoveryDone(
                DiscoveryEnd(search: d.search, count: d.count ?? 0, stopped: d.stopped ?? false, errors: d.errors ?? []))
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
        case "tile_error":
            guard let tile = w.tile else { return nil }
            return .tileError(tile: tile, reason: w.reason, status: w.status)
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
        case "talk":
            guard let tile = w.tile, let state = w.state else { return nil }
            return .talk(
                TalkEvent(
                    tile: tile, device: w.device, channel: w.channel, state: TalkEvent.State(state), reason: w.reason,
                    status: w.status, retryMilliseconds: w.retry_ms, sentMilliseconds: w.sent_ms,
                    droppedMilliseconds: w.dropped_ms))
        case "timing":
            guard let what = w.what, let ns = w.ns else { return nil }
            var detail: TierSwitchTiming?
            if what == "tier_switch", w.delta_ms != nil || w.held_ms != nil || w.capped != nil {
                detail = TierSwitchTiming(
                    deltaMs: w.delta_ms.map { Int($0.rounded()) }, heldMs: Int((w.held_ms ?? 0).rounded()),
                    capped: w.capped ?? false)
            }
            return .timing(what: what, tile: w.tile, nanos: ns, tierSwitch: detail)
        default:
            return nil
        }
    }
}

/// How a tier switch lined up the new generation with the old (the
/// `tier_switch` timing's extra fields, docs/FFI.md).
public struct TierSwitchTiming: Sendable, Equatable {
    /// The shown new picture's timestamp minus the old generation's latest
    /// decoded one, in ms (negative: stepped back in time); nil when nothing
    /// of the old generation was decoded.
    public var deltaMs: Int?
    /// How long the switch waited for alignment, in ms.
    public var heldMs: Int
    /// The wait hit the core's cap.
    public var capped: Bool

    /// A short readout for the frame-rate line: "switch +40 ms" (the new
    /// picture was that far ahead), "switch held 160 ms" (waited for it to
    /// catch up), "capped −520 ms" (waited the most and stepped back).
    public var note: String {
        func signed(_ v: Int) -> String { (v < 0 ? "\u{2212}" : "+") + "\(abs(v)) ms" }
        if capped { return deltaMs.map { "capped " + signed($0) } ?? "capped" }
        if heldMs > 0 { return "switch held \(heldMs) ms" }
        return deltaMs.map { "switch " + signed($0) } ?? "switch"
    }

    public init(deltaMs: Int? = nil, heldMs: Int = 0, capped: Bool = false) {
        self.deltaMs = deltaMs
        self.heldMs = heldMs
        self.capped = capped
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

/// Who detected something: the camera's own AI, a model on this Mac, or the
/// sound listener (`local_audio`: patterns heard in a tile's audio).
public enum DetectionSource: Sendable, Equatable, Hashable {
    case device
    case local
    case localAudio
    case other(String)

    public init(_ s: String) {
        switch s {
        case "device": self = .device
        case "local", "local_video": self = .local
        case "local_audio": self = .localAudio
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

/// One `talk` event: where a tile's talk-back stands.
public struct TalkEvent: Sendable, Equatable {
    public enum State: Sendable, Equatable {
        case starting, active, refused, stopped
        case other(String)

        public init(_ s: String) {
            switch s {
            case "starting": self = .starting
            case "active": self = .active
            case "refused": self = .refused
            case "stopped": self = .stopped
            default: self = .other(s)
            }
        }
    }

    public var tile: UInt64
    public var device: UInt64?
    public var channel: Int?
    public var state: State
    /// With `refused` and `stopped`: `not_permitted`, `rate_limited`,
    /// `device_refused`, `stopped`, `idle`, ...
    public var reason: String?
    /// The device's reply status, when it gave one.
    public var status: Int?
    /// With `rate_limited`: how long until a start is allowed again.
    public var retryMilliseconds: Int?
    public var sentMilliseconds: Int?
    public var droppedMilliseconds: Int?

    public init(
        tile: UInt64, device: UInt64? = nil, channel: Int? = nil, state: State, reason: String? = nil,
        status: Int? = nil, retryMilliseconds: Int? = nil, sentMilliseconds: Int? = nil,
        droppedMilliseconds: Int? = nil
    ) {
        self.tile = tile
        self.device = device
        self.channel = channel
        self.state = state
        self.reason = reason
        self.status = status
        self.retryMilliseconds = retryMilliseconds
        self.sentMilliseconds = sentMilliseconds
        self.droppedMilliseconds = droppedMilliseconds
    }
}
