import Foundation

// The smart-alert event log as the Events window shows it (docs/UX.md
// "Smart alerts", "Events"): the entries `nv_alerts_get` answers, the
// filters and the query they make, feedback and mutes. Pure.

/// One logged decision.
public struct SmartAlertLogEntry: Sendable, Equatable, Identifiable {
    public var loggedMs: Int64
    public var source: String
    public var triggerID: UInt64
    public var decidedMs: Int64
    public var camera: AlertCamera?
    public var className: String?
    /// `alert`, `quiet`, or `suppress`.
    public var decision: String
    public var severity: String?
    public var reason: String?
    public var explanation: String
    public var alertID: UInt64?
    public var unverified: Bool
    public var late: Bool
    public var always: String?
    public var steps: [TraceStep]
    public var deliveries: [TraceStep]
    public var hasSnapshot: Bool
    /// `up` or `down`, and the reason given with `down`.
    public var vote: String?
    public var voteReason: String?

    public var id: String { "\(triggerID)-\(alertID ?? 0)-\(decidedMs)" }

    public init?(_ json: JSONValue) {
        guard let d = json["decision"], let t = d["trigger_id"]?.int64, let kind = d["decision"]?.string else {
            return nil
        }
        loggedMs = json["logged_ms"]?.int64 ?? d["decided_ms"]?.int64 ?? 0
        source = json["source"]?.string ?? "device"
        triggerID = UInt64(max(t, 0))
        decidedMs = d["decided_ms"]?.int64 ?? loggedMs
        camera = d["camera"]?.alertCamera
        className = d["class"]?.string
        decision = kind
        severity = d["severity"]?.string
        reason = d["reason"]?.string
        explanation = d["explanation"]?.string ?? ""
        alertID = d["alert_id"]?.int64.map { UInt64(max($0, 0)) }
        unverified = d["unverified"]?.bool ?? false
        late = d["late"]?.bool ?? false
        always = d["always"]?.string
        steps = (d["trace"]?["steps"]?.array ?? []).compactMap(TraceStep.init)
        deliveries = (json["deliveries"]?.array ?? []).compactMap { TraceStep($0) ?? $0["step"].flatMap(TraceStep.init) }
        hasSnapshot = json["snapshot"]?.bool ?? false
        vote = json["feedback"]?["vote"]?.string
        voteReason = json["feedback"]?["reason"]?.string
    }

    /// The explanation's first sentence (the list's line).
    public var firstSentence: String {
        let text = explanation.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let r = text.range(of: ". ") else { return text }
        return String(text[..<r.lowerBound]) + "."
    }

    /// Where the trigger came from, in the filter's words.
    public var sourceGroup: AlertSourceGroup { AlertSourceGroup.of(source) }

    /// The whole trace, the deliveries the decision did not carry added.
    public var allSteps: [TraceStep] {
        let have = Set(steps.filter { $0.step == "delivery" }.map { $0.fields["output"]?.int64 ?? -1 })
        return steps + deliveries.filter { !have.contains($0.fields["output"]?.int64 ?? -1) }
    }
}

/// The filter's sources.
public enum AlertSourceGroup: String, CaseIterable, Sendable {
    case cameraAI, sound, sensor

    public var title: String {
        switch self {
        case .cameraAI: "Camera AI"
        case .sound: "Sound"
        case .sensor: "Sensor"
        }
    }

    /// The core's source names in this group.
    public var sources: [String] {
        switch self {
        case .cameraAI: ["device", "nvr_input", "local_motion"]
        case .sound: ["local_audio"]
        case .sensor: ["external"]
        }
    }

    public static func of(_ source: String) -> AlertSourceGroup {
        allCases.first { $0.sources.contains(source) } ?? .cameraAI
    }
}

/// A page of the log.
public struct SmartAlertLogPage: Sendable, Equatable {
    public var entries: [SmartAlertLogEntry]
    public var more: Bool
    /// The service's status (`configured`, `enabled`, `muted`, `inputs`,
    /// `outputs`, ...), as sent.
    public var status: JSONValue?

    public init(entries: [SmartAlertLogEntry] = [], more: Bool = false, status: JSONValue? = nil) {
        self.entries = entries
        self.more = more
        self.status = status
    }

    public static func parse(_ text: String) -> SmartAlertLogPage? {
        guard let v = JSONValue.parse(text), let list = v["entries"]?.array else { return nil }
        return SmartAlertLogPage(entries: list.compactMap(SmartAlertLogEntry.init), more: v["more"]?.bool ?? false, status: v["status"])
    }

    /// The cameras muted now and until when (ms since 1970).
    public var muted: [(camera: AlertCamera, untilMs: Int64?)] {
        (status?["muted"]?.array ?? []).compactMap { m in
            guard let c = (m["camera"] ?? m).alertCamera else { return nil }
            return (c, m["until_ms"]?.int64)
        }
    }
}

/// The Events window's filters and the query they make.
public struct SmartAlertLogFilter: Sendable, Equatable {
    public var camera: AlertCamera?
    public var sources: Set<AlertSourceGroup> = []
    public var decisions: Set<String> = []
    public var classes: Set<String> = []
    public var minSeverity: AlertSeverity?
    public var sinceMs: Int64?
    public var untilMs: Int64?
    public var text = ""
    public var limit = 200

    public init() {}

    public static let decisionKinds = ["alert", "quiet", "suppress"]

    /// The `nv_alerts_get` query (docs/FFI.md "The event log"); `beforeMs`
    /// pages back from the last entry's decision time.
    public func query(beforeMs: Int64? = nil) -> String {
        var q: [String: Any] = ["limit": min(max(limit, 1), 1000)]
        if let camera { q["camera"] = ["device": camera.device, "channel": camera.channel, "lens": camera.lens] }
        if !sources.isEmpty { q["source"] = sources.flatMap(\.sources).sorted() }
        if !decisions.isEmpty { q["decision"] = decisions.sorted() }
        if !classes.isEmpty { q["class"] = classes.sorted() }
        if let minSeverity { q["min_severity"] = minSeverity.rawValue }
        if let sinceMs { q["since_ms"] = sinceMs }
        if let untilMs { q["until_ms"] = untilMs }
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !t.isEmpty { q["text"] = t }
        if let beforeMs { q["before_ms"] = beforeMs }
        let data = (try? JSONSerialization.data(withJSONObject: q, options: [.sortedKeys])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    /// The same filter on an entry (a core answers the query itself; this
    /// keeps a list consistent as new decisions arrive between queries).
    public func matches(_ e: SmartAlertLogEntry) -> Bool {
        if let camera, e.camera?.device != camera.device || e.camera?.channel != camera.channel
            || (e.camera?.lens ?? 0) != camera.lens
        {
            return false
        }
        if !sources.isEmpty, !sources.contains(e.sourceGroup) { return false }
        if !decisions.isEmpty, !decisions.contains(e.decision) { return false }
        if !classes.isEmpty, !classes.contains(e.className ?? "") { return false }
        if let minSeverity {
            guard let s = e.severity.flatMap(AlertSeverity.init(rawValue:)), s >= minSeverity else { return false }
        }
        if let sinceMs, e.decidedMs < sinceMs { return false }
        if let untilMs, e.decidedMs >= untilMs { return false }
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !t.isEmpty, e.explanation.range(of: t, options: [.caseInsensitive, .diacriticInsensitive]) == nil {
            return false
        }
        return true
    }
}

/// Feedback on an alert (docs/NOTIFICATIONS.md "Feedback").
public enum AlertFeedbackReason: String, CaseIterable, Sendable {
    case nothingThere = "nothing_there"
    case wrongType = "wrong_type"
    case notInteresting = "not_interesting"
    case duplicate

    public var title: String {
        switch self {
        case .nothingThere: "Nothing there"
        case .wrongType: "Wrong type"
        case .notInteresting: "Real, but not interesting here or now"
        case .duplicate: "Duplicate"
        }
    }

    /// `{"alert_id","vote","reason"?}`: a reason only with a down vote.
    public static func json(alertID: UInt64, useful: Bool, reason: AlertFeedbackReason? = nil) -> String {
        var o: [String: Any] = ["alert_id": alertID, "vote": useful ? "up" : "down"]
        if !useful, let reason { o["reason"] = reason.rawValue }
        let data = (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// Muting a camera (the only thing that stops always-alert rules; at most
/// 24 h).
public enum AlertMuteChoice: Hashable, Sendable, CaseIterable {
    case minutes15, hour1, untilMorning

    public var title: String {
        switch self {
        case .minutes15: "15 Minutes"
        case .hour1: "1 Hour"
        case .untilMorning: "Until Morning"
        }
    }

    /// Seconds from `now`: "until morning" is the next 07:00 local time.
    public func seconds(now: Date, calendar: Calendar = .current) -> UInt32 {
        switch self {
        case .minutes15: return 15 * 60
        case .hour1: return 3600
        case .untilMorning:
            let next =
                calendar.nextDate(
                    after: now, matching: DateComponents(hour: 7, minute: 0, second: 0), matchingPolicy: .nextTime)
                ?? now.addingTimeInterval(3600)
            return UInt32(min(max(next.timeIntervalSince(now).rounded(), 60), 86_400))
        }
    }
}
