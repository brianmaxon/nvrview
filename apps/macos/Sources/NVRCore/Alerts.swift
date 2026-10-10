import Foundation

// Alerts (docs/UX.md "Alerts"): some detections must not pass unseen. A pure
// model, tested without a window; never an "all clear".

/// Which detections raise an alert: one list, so device alarms can be added
/// later. Today the sound alarms heard in a tile's audio.
public enum AlertPolicy {
    public static let kinds: Set<String> = ["smoke_alarm", "co_alarm"]
    public static let sources: Set<DetectionSource> = [.localAudio]

    public static func raises(kind: String, source: DetectionSource) -> Bool {
        kinds.contains(kind) && sources.contains(source)
    }
}

/// The red border's behavior: blinking about once a second, or steady with
/// the system's Reduce Motion setting.
public enum AlertBorder {
    public enum Mode: Sendable, Equatable { case blinking, steady }

    public static let blinkPeriod = 1.0

    public static func mode(reduceMotion: Bool) -> Mode { reduceMotion ? .steady : .blinking }

    /// Whether the border is drawn `seconds` into the alert (for the layer
    /// animation's timing and for tests): on half the period when blinking.
    public static func visible(_ mode: Mode, at seconds: Double) -> Bool {
        switch mode {
        case .steady: true
        case .blinking: seconds.truncatingRemainder(dividingBy: blinkPeriod) < blinkPeriod / 2
        }
    }
}

/// One raised alert for a camera (a device's channel: every lens).
public struct Alert: Sendable, Equatable {
    public var device: String
    public var channel: Int
    public var kind: String
    public var firstAt: Date
    public var lastAt: Date
    /// Detection starts since it was raised.
    public var count: Int
    /// Whether the detection is still going.
    public var sounding: Bool

    /// "Smoke alarm (sound)".
    public var reason: String {
        "\(DetectionTracker.label(kind)) (\(sourceWord))"
    }
    private var sourceWord: String { "sound" }

    /// The header's lines: reason, first detection, count and last, state.
    public func lines(now: Date, calendar: Calendar = .current, locale: Locale = .current) -> [String] {
        func stamp(_ d: Date) -> String {
            let f = DateFormatter()
            f.locale = locale
            f.calendar = calendar
            f.timeZone = calendar.timeZone
            f.timeStyle = .medium
            f.dateStyle = calendar.isDate(d, inSameDayAs: now) ? .none : .medium
            return f.string(from: d)
        }
        let times = count == 1 ? "1 time" : "\(count) times"
        return [
            reason,
            "First detected \(stamp(firstAt))",
            "Detected \(times), last \(stamp(lastAt))",
            sounding ? "Sounding now" : "Not sounding now",
        ]
    }
}

/// A line for the session's event log.
public struct AlertLogEntry: Sendable, Equatable {
    public var at: Date
    public var text: String
}

/// The session's alerts, by camera (device and channel). Detection starts
/// raise or count; stops only mark it quiet; only a dismissal clears it.
public struct AlertCenter: Sendable, Equatable {
    private struct Key: Hashable { var device: String; var channel: Int }
    private var alerts: [Key: Alert] = [:]
    /// Raised, counted-and-dismissed history for the session, oldest first.
    public private(set) var log: [AlertLogEntry] = []

    public init() {}

    private static func key(_ camera: CameraRef) -> Key { Key(device: camera.device, channel: camera.channel) }

    /// The alert for any lens of a channel's camera.
    public func alert(for camera: CameraRef) -> Alert? { alerts[Self.key(camera)] }
    public func isActive(_ camera: CameraRef) -> Bool { alert(for: camera) != nil }
    public var all: [Alert] { alerts.values.sorted { $0.firstAt < $1.firstAt } }

    /// Folds in a detection event. `at` is when the sound was first heard
    /// (a start arrives seconds later). Returns whether anything changed.
    @discardableResult
    public mutating func apply(_ event: DetectionEvent, deviceKey: String, at: Date) -> Bool {
        guard !event.alarm, AlertPolicy.raises(kind: event.kind, source: event.source) else { return false }
        let key = Key(device: deviceKey, channel: event.channel)
        if event.active {
            if var a = alerts[key] {
                a.count += 1
                a.lastAt = at
                a.sounding = true
                a.kind = event.kind
                alerts[key] = a
                log.append(.init(at: at, text: "Alert again: \(a.reason), channel \(event.channel + 1), detection \(a.count)"))
            } else {
                let a = Alert(
                    device: deviceKey, channel: event.channel, kind: event.kind, firstAt: at, lastAt: at, count: 1,
                    sounding: true)
                alerts[key] = a
                log.append(.init(at: at, text: "Alert raised: \(a.reason), channel \(event.channel + 1)"))
            }
            return true
        }
        guard var a = alerts[key], a.sounding else { return false }
        a.sounding = false
        alerts[key] = a
        return true
    }

    /// A lost session says nothing about whether it is still sounding.
    public mutating func deviceLost(_ device: String) {
        for (k, var a) in alerts where k.device == device && a.sounding {
            a.sounding = false
            alerts[k] = a
        }
    }

    /// Adds a line to the session's event log (a doorbell call, say).
    public mutating func record(_ text: String, at: Date) {
        log.append(.init(at: at, text: text))
    }

    /// The user dismissed the camera's alert: gone for every tile and window,
    /// count reset. Returns the dismissed alert.
    @discardableResult
    public mutating func dismiss(_ camera: CameraRef, at: Date) -> Alert? {
        guard let a = alerts.removeValue(forKey: Self.key(camera)) else { return nil }
        let times = a.count == 1 ? "1 detection" : "\(a.count) detections"
        log.append(.init(at: at, text: "Alert dismissed: \(a.reason), channel \(a.channel + 1), \(times)"))
        return a
    }
}
