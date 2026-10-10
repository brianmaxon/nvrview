import Foundation

// Small pure pieces of the viewer's state, tested without a window:
// detections per camera, who is listening, and what a tile says while its
// device is not streaming.

// MARK: Detections

/// Which detections are active per camera, from `detection` and `motion`
/// events (docs/FFI-ABI3.md section 2).
public struct DetectionTracker: Sendable, Equatable {
    public struct Active: Sendable, Equatable {
        public var kind: String
        public var source: DetectionSource
        /// When it started, on the core clock (or 0 when the event had none).
        public var sinceNanos: UInt64
    }

    private var active: [CameraRef: [Active]] = [:]
    /// Sound detections belong to a channel's microphone, not to a lens, so
    /// they are kept per channel (keyed by its first lens) and shown for
    /// every lens of the channel.
    private var sound: [CameraRef: [Active]] = [:]
    var activeKeys: [CameraRef] { Array(active.keys) }
    var soundKeys: [CameraRef] { Array(sound.keys) }

    public init() {}

    /// Folds in one event for the camera it names.
    public mutating func apply(_ event: DetectionEvent, camera: CameraRef) {
        let isSound = event.source == .localAudio
        let key = isSound ? Self.channelKey(camera) : camera
        var list = (isSound ? sound[key] : active[key]) ?? []
        list.removeAll { $0.kind == event.kind && $0.source == event.source }
        if event.active {
            list.append(Active(kind: event.kind, source: event.source, sinceNanos: event.atNanos ?? 0))
        }
        list.sort { Self.order($0.kind) < Self.order($1.kind) }
        if isSound { sound[key] = list.isEmpty ? nil : list } else { active[key] = list.isEmpty ? nil : list }
    }

    private static func channelKey(_ camera: CameraRef) -> CameraRef {
        CameraRef(device: camera.device, channel: camera.channel, lens: 0)
    }

    /// Forgets a device's detections (it disconnected: its stops may never come).
    public mutating func clear(device: String) {
        active = active.filter { $0.key.device != device }
        sound = sound.filter { $0.key.device != device }
    }

    /// What is active for one camera (one lens): its own detections, then
    /// the channel's sound detections, which every lens shows.
    public func active(for camera: CameraRef) -> [Active] {
        let own = active[camera] ?? []
        let heard = sound[Self.channelKey(camera)] ?? []
        return (own + heard).sorted { Self.order($0.kind) < Self.order($1.kind) }
    }

    /// Drops detections older than `maxAgeNanos` (a lost stop must not leave
    /// a badge up forever).
    public mutating func expire(now: UInt64, maxAgeNanos: UInt64) {
        for (camera, list) in active {
            let kept = list.filter { $0.sinceNanos == 0 || now &- $0.sinceNanos < maxAgeNanos }
            active[camera] = kept.isEmpty ? nil : kept
        }
        for (camera, list) in sound {
            let kept = list.filter { $0.sinceNanos == 0 || now &- $0.sinceNanos < maxAgeNanos }
            sound[camera] = kept.isEmpty ? nil : kept
        }
    }

    /// Objects first, motion last.
    private static func order(_ kind: String) -> Int {
        switch kind {
        case "people": 0
        case "vehicle": 1
        case "dog_cat": 2
        case "face": 3
        case "package": 4
        case "smoke_alarm", "co_alarm": 6
        case DetectionEvent.motionKind: 9
        default: 5
        }
    }

    /// The label a badge shows for a device's kind name.
    public static func label(_ kind: String) -> String {
        switch kind {
        case "people", "person": "Person"
        case "vehicle": "Vehicle"
        case "dog_cat", "animal": "Animal"
        case "face": "Face"
        case "package": "Package"
        case DetectionEvent.motionKind: "Motion"
        case "smoke_alarm": "Smoke alarm"
        case "co_alarm": "CO alarm"
        default:
            kind.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    /// The text of a tile badge: the label, then where it came from when that
    /// is not the camera's own AI ("Mac" for this computer's vision, "sound"
    /// for the audio listener), then the lens of a multi-lens camera.
    public static func badgeText(_ a: Active, lensName: String? = nil) -> String {
        var text = label(a.kind)
        switch a.source {
        case .local: text += " · Mac"
        case .localAudio: text += " · sound"
        default: break
        }
        if let lensName, a.source != .localAudio { text += " · " + lensName }
        return text
    }

    /// Where it was detected, in words.
    public static func sourceLabel(_ source: DetectionSource) -> String {
        switch source {
        case .device: "camera AI"
        case .local: "this Mac"
        case .localAudio: "sound"
        case .other(let s): s
        }
    }
}

// MARK: Listening

/// Which cameras are audible (docs/UX.md "Listening from any tile"): one
/// click on a tile's speaker makes that camera the only audible one, or
/// mutes it if it is already audible; with the modifier held it is added to
/// (or taken from) the audible set instead.
public enum ListenPolicy {
    public static func toggle(_ camera: CameraRef, in audible: Set<CameraRef>, additive: Bool) -> Set<CameraRef> {
        if audible.contains(camera) { return audible.subtracting([camera]) }
        return additive ? audible.union([camera]) : [camera]
    }
}

// MARK: What a tile says

/// A device's connection state with the time it arrived, so a backoff can
/// count down.
public struct DeviceLink: Sendable, Equatable {
    public var state: DeviceState?
    /// When `state` arrived, seconds on a monotonic clock.
    public var since: TimeInterval

    public init(state: DeviceState? = nil, since: TimeInterval = 0) {
        self.state = state
        self.since = since
    }
}

/// The words over a tile that is not showing live video, or nil when it is.
public struct TileVeil: Sendable, Equatable {
    public var title: String
    public var detail: String?

    public init(title: String, detail: String? = nil) {
        self.title = title
        self.detail = detail
    }

    /// `now` on the same clock as `link.since`; `status` is the tile's last
    /// status action (nil before the tile exists).
    public static func make(link: DeviceLink, status: TileStatus?, hasPicture: Bool, now: TimeInterval)
        -> TileVeil?
    {
        switch link.state {
        case nil, .connecting?:
            return TileVeil(title: "Connecting")
        case .backoff(let attempt, let retry)?:
            let remaining = max(Double(retry) / 1000 - (now - link.since), 0)
            let seconds = Int(remaining.rounded(.up))
            let when = seconds > 0 ? "retrying in \(seconds) s" : "retrying now"
            return TileVeil(title: "Reconnecting", detail: "Connection lost · attempt \(attempt) · \(when)")
        case .loginFailed(_, let remain, let reason, let unlock)?:
            var detail = reason.map { "\($0.prefix(1).uppercased())\($0.dropFirst())" } ?? "Check the credentials in Settings"
            if let unlock, unlock > 0 {
                detail += " · locked for \((unlock + 59) / 60) min"
            } else if let remain {
                detail += " · \(remain) attempt\(remain == 1 ? "" : "s") left"
            }
            return TileVeil(title: "Login failed", detail: detail)
        case .closed?:
            return TileVeil(title: "Disconnected")
        case .other(let s)?:
            return TileVeil(title: s.capitalized)
        case .connected?:
            switch status {
            case nil, .waiting?:
                return hasPicture ? nil : TileVeil(title: "Waiting for video")
            case .playing?:
                return nil
            case .recovering?:
                // The last picture stays; a small note only if there is none.
                return hasPicture ? nil : TileVeil(title: "Recovering")
            case .undecodable?:
                return TileVeil(title: "Cannot decode this stream")
            }
        }
    }
}

/// The frame-rate readout of a tile header.
public enum FrameRateText {
    /// `switchNote` (`TierSwitchTiming.note`) is the last tier switch, while
    /// it is recent.
    public static func make(device: Double?, shown: Double?, displayHz: Int, switchNote: String? = nil) -> String {
        func fps(_ v: Double?) -> String { v.map { String(format: "%.1f fps", $0) } ?? "– fps" }
        var parts = ["Device \(fps(device))", "Shown \(fps(shown))"]
        if displayHz > 0 { parts.append("\(displayHz) Hz") }
        if let switchNote { parts.append(switchNote) }
        return parts.joined(separator: " · ")
    }
}

extension DetectionEvent {
    /// The camera (lens included) whose tiles show this detection: `lens` 1
    /// is the second lens of a multi-lens camera; absent means the first.
    public func camera(deviceKey: String) -> CameraRef {
        CameraRef(device: deviceKey, channel: channel, lens: min(max(lens ?? 0, 0), CameraRef.maxLens))
    }
}

/// How a tile shows its audio level meter (docs/UX.md "Listening from any
/// tile"): along the top of the tile, green and always visible while the
/// tile is audible; monochrome, with the header's hover fade, while muted;
/// not at all without a camera.
public enum LevelMeterMode: Sendable, Equatable {
    case hidden
    /// Shown only while the header is (hovered); white on gray.
    case muted
    /// Always shown; green.
    case audible

    public init(hasCamera: Bool, audible: Bool) {
        self = !hasCamera ? .hidden : (audible ? .audible : .muted)
    }

    /// Whether the meter is on screen, given whether the header is.
    public func visible(headerShown: Bool) -> Bool {
        switch self {
        case .hidden: false
        case .muted: headerShown
        case .audible: true
        }
    }

    /// Whether to read the tile's level this refresh: only for meters on
    /// screen, so hidden tiles cost nothing.
    public func polls(headerShown: Bool) -> Bool { visible(headerShown: headerShown) }
}

/// The sidebar's "Hide Offline Channels" filter: an NVR advertises every
/// slot it has, most of them empty on a large unit. Only the sidebar list is
/// filtered; layouts and groups refer to cameras by device and channel, so
/// they keep working whatever is hidden.
public enum ChannelFilter {
    public static func visible(_ channels: [ChannelInfo], hideOffline: Bool) -> [ChannelInfo] {
        hideOffline ? channels.filter(\.connected) : channels
    }

    /// "16" when all are listed, "4 of 16" when some are hidden, nil without channels.
    public static func countText(shown: Int, total: Int) -> String? {
        guard total > 0 else { return nil }
        return shown == total ? "\(total)" : "\(shown) of \(total)"
    }
}
