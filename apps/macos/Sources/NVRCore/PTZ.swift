import Foundation

// Camera motion as the app sees it (docs/FFI.md "Camera motion (PTZ)",
// docs/CAPTURE.md "Pan, tilt, and zoom", docs/UX.md "Pan, tilt, and zoom"):
// the answer of `nv_ptz_info`, the `ptz` events, the keys, the held-move
// timing and the lens arithmetic. Pure, so both the real core and the fake
// are driven the same way and the rules are tested headless.

/// A pan or tilt direction (`nv_ptz_move`).
public enum PTZDirection: String, Sendable, CaseIterable {
    case left, right, up, down
}

/// A lens motor (`nv_ptz_lens`).
public enum PTZLensAxis: String, Sendable, CaseIterable {
    case zoom, focus
}

/// One lens motor's range and position, in the camera's own steps (294).
public struct PTZRange: Sendable, Equatable, Decodable {
    public var min: Int
    public var max: Int
    public var cur: Int

    public init(min: Int, max: Int, cur: Int) {
        self.min = min
        self.max = max
        self.cur = cur
    }

    /// `position` limited to the range.
    public func clamped(_ position: Int) -> Int { Swift.min(Swift.max(position, min), Swift.max(max, min)) }

    /// How far `position` is along the range, 0...1.
    public func fraction(_ position: Int) -> Double {
        guard max > min else { return 0 }
        return Double(clamped(position) - min) / Double(max - min)
    }

    /// The position `fraction` of the way along the range.
    public func position(at fraction: Double) -> Int {
        guard max > min else { return min }
        return clamped(min + Int((fraction * Double(max - min)).rounded()))
    }

    /// The position one key step from `position`: a twentieth of the range
    /// (at least 1), limited to the range.
    public func stepped(from position: Int, by direction: Int) -> Int {
        let step = Swift.max(1, (max - min) / 20)
        return clamped(position + (direction >= 0 ? step : -step))
    }
}

/// One preset of the camera's list (190); recall only.
public struct PTZPreset: Sendable, Equatable, Decodable {
    public var id: Int
    public var name: String

    public init(id: Int, name: String) {
        self.id = id
        self.name = name
    }
}

/// What `nv_ptz_info` answers for a tile's camera.
public struct PTZInfo: Sendable, Equatable, Decodable {
    public var panTilt: Bool
    public var zoom: PTZRange?
    public var focus: PTZRange?
    public var presets: [PTZPreset]
    public var minSpeed: Int
    public var maxSpeed: Int

    private enum CodingKeys: String, CodingKey {
        case panTilt = "pan_tilt"
        case zoom, focus, presets, speeds
    }

    public init(
        panTilt: Bool = false, zoom: PTZRange? = nil, focus: PTZRange? = nil, presets: [PTZPreset] = [],
        minSpeed: Int = 1, maxSpeed: Int = 64
    ) {
        self.panTilt = panTilt
        self.zoom = zoom
        self.focus = focus
        self.presets = presets
        self.minSpeed = minSpeed
        self.maxSpeed = maxSpeed
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        panTilt = (try? c.decodeIfPresent(Bool.self, forKey: .panTilt)) ?? false
        zoom = try? c.decodeIfPresent(PTZRange.self, forKey: .zoom)
        focus = try? c.decodeIfPresent(PTZRange.self, forKey: .focus)
        presets = (try? c.decodeIfPresent([PTZPreset].self, forKey: .presets)) ?? []
        let speeds = (try? c.decodeIfPresent([Int].self, forKey: .speeds)) ?? [1, 64]
        minSpeed = speeds.first ?? 1
        maxSpeed = speeds.count > 1 ? speeds[1] : 64
    }

    /// Parses the JSON of `nv_ptz_info`; nil when it is not one.
    public static func parse(_ json: String) -> PTZInfo? {
        try? JSONDecoder().decode(PTZInfo.self, from: Data(json.utf8))
    }

    public var hasLens: Bool { zoom != nil || focus != nil }

    public func range(_ axis: PTZLensAxis) -> PTZRange? { axis == .zoom ? zoom : focus }
}

/// A `ptz` event: what became of one command (docs/FFI.md).
public struct PTZEvent: Sendable, Equatable {
    public enum State: Sendable, Equatable {
        case sent, refused, failed
        case other(String)

        public init(_ s: String) {
            switch s {
            case "sent": self = .sent
            case "refused": self = .refused
            case "failed": self = .failed
            default: self = .other(s)
            }
        }
    }

    public var tile: UInt64
    /// `move`, `stop`, `preset`, `lens`.
    public var action: String
    public var state: State
    /// With `refused`, `failed`, and a `stop` the core sent itself
    /// (`released`, `deadman`, `max_hold`, ...).
    public var reason: String?
    public var status: Int?
    /// With a `lens` that was sent: the position, after the core's clamp.
    public var position: Int?
    public var retryMilliseconds: Int?

    public init(
        tile: UInt64, action: String, state: State, reason: String? = nil, status: Int? = nil, position: Int? = nil,
        retryMilliseconds: Int? = nil
    ) {
        self.tile = tile
        self.action = action
        self.state = state
        self.reason = reason
        self.status = status
        self.position = position
        self.retryMilliseconds = retryMilliseconds
    }

    /// A short line for the popover and the message over the tile, nil for
    /// an event that needs none.
    public var problem: String? {
        switch state {
        case .sent, .other: return nil
        case .refused:
            switch reason {
            case "rate_limited": return "Moving too fast; try again in a moment"
            case "not_supported": return "This camera cannot do that"
            case "unknown_preset": return "That preset is no longer on the camera"
            case "max_hold": return "Stopped after 30 seconds; release and press again"
            default: return "The camera was not moved (\(reason ?? "refused"))"
            }
        case .failed:
            switch reason {
            case "device_refused": return "The camera refused the command" + (status.map { " (status \($0))" } ?? "")
            case "connection_lost", "session_closed": return "The connection to the camera was lost"
            case "timed_out": return "The camera did not answer"
            default: return "The command failed (\(reason ?? "failed"))"
            }
        }
    }
}

// MARK: Held moves

/// The timing of a held pan or tilt: the app calls `nv_ptz_move` again at
/// least every 500 ms while the control is held (the core stops the camera
/// 1 s after the last call); release calls `nv_ptz_stop`. Pure, on any
/// monotonic clock.
public struct PTZHold: Sendable, Equatable {
    /// How often a held move is renewed (well inside the core's 500 ms).
    public static let renewInterval: Double = 0.25

    /// What to tell the core.
    public enum Call: Sendable, Equatable {
        case move(PTZDirection, speed: Int)
        case stop
    }

    public private(set) var direction: PTZDirection?
    public private(set) var speed = 20
    private var lastCall = 0.0

    public init() {}

    public var isHeld: Bool { direction != nil }

    /// The control goes down (or its direction or speed changes while held).
    public mutating func press(_ direction: PTZDirection, speed: Int, now: Double) -> Call {
        self.direction = direction
        self.speed = speed
        lastCall = now
        return .move(direction, speed: speed)
    }

    /// The control comes up: a stop when something was held.
    public mutating func release() -> Call? {
        guard direction != nil else { return nil }
        direction = nil
        return .stop
    }

    /// Time passes: the renewal when it is due.
    public mutating func tick(now: Double) -> Call? {
        guard let d = direction, now - lastCall >= Self.renewInterval else { return nil }
        lastCall = now
        return .move(d, speed: speed)
    }
}

/// The names automation uses for `nv_motion_grant`.
public enum MotionClient {
    /// Automatic correction (docs/ARCHITECTURE.md "Device commands").
    public static let autoCorrection = "auto_correction"
}

/// A drag on a lens slider sends a position at most five times a second and
/// once when it ends (docs/UX.md "Pan, tilt, and zoom").
public struct PTZLensThrottle: Sendable, Equatable {
    public static let interval: Double = 0.2

    private var lastSent: Double?
    private var pending: Int?

    public init() {}

    /// The slider is at `position` at `now`: the position to send now, if
    /// the interval has passed (otherwise it waits for `tick` or `end`).
    public mutating func moved(to position: Int, now: Double) -> Int? {
        if let last = lastSent, now - last < Self.interval {
            pending = position
            return nil
        }
        lastSent = now
        pending = nil
        return position
    }

    /// Time passes during the drag: a waiting position once the interval
    /// has passed.
    public mutating func tick(now: Double) -> Int? {
        guard let p = pending, let last = lastSent, now - last >= Self.interval else { return nil }
        lastSent = now
        pending = nil
        return p
    }

    /// The drag ends: the last position, if it was not sent.
    public mutating func end() -> Int? {
        defer {
            lastSent = nil
            pending = nil
        }
        return pending
    }
}

// MARK: Keys

/// What a camera key does to the selected tile's camera (docs/UX.md
/// "Capture and camera keys").
public enum PTZKey: Sendable, Equatable {
    /// A plain arrow on a tile that pans and tilts: moves while held.
    case move(PTZDirection)
    /// `=` or `-` (alone or with Option): lens zoom in (+1) or out (-1).
    case zoom(Int)
    /// Option with 1 to 9: recalls preset 1 to 9 (the order of the camera's list).
    case preset(Int)
}

extension ViewerKeys {
    /// Virtual key codes of `=`, `-` and the digits 1 to 9, by position.
    static let equalKey: UInt16 = 24
    static let minusKey: UInt16 = 27
    static let digitKeys: [UInt16: Int] = [18: 1, 19: 2, 20: 3, 21: 4, 23: 5, 22: 6, 26: 7, 28: 8, 25: 9]

    /// The camera key for a key press, read by key position so it works on
    /// every layout, never while text is edited.
    ///
    /// - A plain arrow (no modifier) is a move when the tile pans and tilts
    ///   (`canMove`); otherwise, and with Option, it is the selection's
    ///   (`shortcut`). Arrows repeat (the hold is renewed by the timer, so a
    ///   repeat changes nothing).
    /// - `=` and `-` zoom, alone when the tile has a zoom lens (`canZoom`)
    ///   and with Option always; digits recall presets with Option only.
    ///   These ignore repeats.
    public static func ptzKey(
        keyCode: UInt16, modifiers: KeyModifiers, isRepeat: Bool, context: KeyContext,
        canMove: Bool, canZoom: Bool
    ) -> PTZKey? {
        guard context != .textEditing, modifiers.isSubset(of: [.option]) else { return nil }
        let option = modifiers == [.option]
        // A plain key is the stage's only; the sidebar keeps its own arrows.
        if !option, context != .stage { return nil }
        if !option, canMove {
            switch keyCode {
            case leftArrow: return .move(.left)
            case rightArrow: return .move(.right)
            case upArrow: return .move(.up)
            case downArrow: return .move(.down)
            default: break
            }
        }
        guard !isRepeat else { return nil }
        if keyCode == equalKey, option || canZoom { return .zoom(1) }
        if keyCode == minusKey, option || canZoom { return .zoom(-1) }
        if option, let n = digitKeys[keyCode] { return .preset(n) }
        return nil
    }

    /// The direction an arrow key's release ends, whatever modifiers are down.
    public static func ptzKeyUp(keyCode: UInt16) -> PTZDirection? {
        switch keyCode {
        case leftArrow: return .left
        case rightArrow: return .right
        case upArrow: return .up
        case downArrow: return .down
        default: return nil
        }
    }
}
