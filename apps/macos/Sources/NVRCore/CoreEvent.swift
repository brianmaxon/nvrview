import Foundation

/// Where a device is in its connection life cycle (`device_state` events).
public enum DeviceState: Sendable, Equatable {
    case connecting
    case connected
    /// Waiting before the next attempt; `retryMilliseconds` from now.
    case backoff(attempt: Int, retryMilliseconds: Int)
    /// Terminal: the core does not retry (accounts lock out). `remainTimes`
    /// is how many further failures the device tolerates, when it says.
    case loginFailed(status: Int?, remainTimes: Int?)
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

    private enum CodingKeys: String, CodingKey { case channel, connected, tiers, name, model }

    public init(
        channel: Int, connected: Bool = false, tiers: [String] = [], name: String? = nil,
        model: String? = nil
    ) {
        self.channel = channel
        self.connected = connected
        self.tiers = tiers
        self.name = name
        self.model = model
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        channel = try c.decode(Int.self, forKey: .channel)
        connected = try c.decodeIfPresent(Bool.self, forKey: .connected) ?? false
        tiers = try c.decodeIfPresent([String].self, forKey: .tiers) ?? []
        name = try c.decodeIfPresent(String.self, forKey: .name)
        model = try c.decodeIfPresent(String.self, forKey: .model)
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

    private struct Wire: Decodable {
        var type: String
        var device: UInt64?
        var tile: UInt64?
        var state: String?
        var attempt: Int?
        var retry_ms: Int?
        var status: Int?
        var remain_times: Int?
        var name: String?
        var model: String?
        var channels: [ChannelInfo]?
        var received: Int?
        var decoded: Int?
        var gated: Int?
        var dropped: Int?
        var failures: Int?
        var resets: Int?
        var reason: String?
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
            case "login_failed": s = .loginFailed(status: w.status, remainTimes: w.remain_times)
            case "closed": s = .closed
            default: s = .other(state)
            }
            return .deviceState(device: device, state: s)
        case "device_info":
            guard let device = w.device else { return nil }
            return .deviceInfo(device: device, name: w.name, model: w.model)
        case "channels":
            guard let device = w.device else { return nil }
            return .channels(device: device, channels: w.channels ?? [])
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
        default:
            return nil
        }
    }
}
