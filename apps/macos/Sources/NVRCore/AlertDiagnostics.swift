import Foundation

// Debugging smart alerts (docs/NOTIFICATIONS.md "Diagnostics", minor 14):
// a sensor's connection state in words, and its message log. Pure.

/// A sensor's broker connection, from the `input_status` entry of the
/// service status (`nv_alerts_get`).
public struct SensorConnection: Sendable, Equatable {
    /// `connecting`, `connected`, `backoff`, `auth_failed`, `refused`, `stopped`.
    public var state: String
    public var lastError: String?
    /// Why it failed: `dns`, `refused`, `timeout`, `network`,
    /// `tls_certificate`, `tls_hostname`, `tls`, `login`, `protocol`,
    /// `not_local`.
    public var errorKind: String?
    public var lastErrorMs: Int64?
    public var lastSuccessMs: Int64?
    public var messages: Int64
    public var triggers: Int64

    public init(
        state: String, lastError: String? = nil, errorKind: String? = nil, lastErrorMs: Int64? = nil,
        lastSuccessMs: Int64? = nil, messages: Int64 = 0, triggers: Int64 = 0
    ) {
        self.state = state
        self.lastError = lastError
        self.errorKind = errorKind
        self.lastErrorMs = lastErrorMs
        self.lastSuccessMs = lastSuccessMs
        self.messages = messages
        self.triggers = triggers
    }

    /// The input entry for the sensor with `id` in the status.
    public static func find(id: String, in status: JSONValue?) -> SensorConnection? {
        guard let entry = (status?["input_status"]?.array)?.first(where: { $0["id"]?.string == id }),
            let s = entry["status"]
        else { return nil }
        return SensorConnection(s)
    }

    public init?(_ s: JSONValue) {
        guard let state = s["status"]?.string else { return nil }
        self.state = state
        lastError = s["last_error"]?.string
        errorKind = s["last_error_kind"]?.string
        lastErrorMs = s["last_error_ms"]?.int64
        lastSuccessMs = s["last_success_ms"]?.int64
        messages = s["messages"]?.int64 ?? 0
        triggers = s["triggers"]?.int64 ?? 0
    }

    /// The kind's short name.
    public static func kindTitle(_ kind: String) -> String {
        switch kind {
        case "dns": "Name lookup failed"
        case "refused": "Connection refused"
        case "timeout": "Timed out"
        case "network": "Network error"
        case "tls_certificate": "TLS certificate"
        case "tls_hostname": "TLS name mismatch"
        case "tls": "TLS error"
        case "login": "Login rejected"
        case "protocol": "Protocol error"
        case "not_local": "Not on the local network"
        default: kind.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    /// The settings screen's line.
    public func line(now: Date = Date(), timeZone: TimeZone = .current, locale: Locale = .current) -> String {
        func stamp(_ ms: Int64) -> String {
            let f = DateFormatter()
            f.locale = locale
            f.timeZone = timeZone
            f.timeStyle = .medium
            let d = Date(timeIntervalSince1970: Double(ms) / 1000)
            var cal = Calendar(identifier: .gregorian)
            cal.timeZone = timeZone
            f.dateStyle = cal.isDate(d, inSameDayAs: now) ? .none : .short
            return f.string(from: d)
        }
        switch state {
        case "connected":
            let count = messages == 1 ? "1 message" : "\(messages) messages"
            return "Connected (\(count) so far)"
        case "connecting" where lastError == nil:
            return "Connecting…"
        case "stopped":
            return "Stopped"
        default:
            var parts: [String] = []
            let title = errorKind.map(Self.kindTitle)
            if let lastError {
                parts.append(title.map { "\($0): \(lastError)" } ?? lastError)
            } else if let title {
                parts.append(title)
            } else {
                parts.append(state.replacingOccurrences(of: "_", with: " ").capitalized)
            }
            if state == "auth_failed" { parts.append("not retried; save the password to try again") }
            parts.append(lastSuccessMs.map { "Last connected \(stamp($0))" } ?? "Never connected")
            return parts.joined(separator: ". ")
        }
    }
}

/// One logged MQTT message.
public struct SensorMessage: Sendable, Equatable, Identifiable {
    public var seq: Int64
    public var atMs: Int64
    public var topic: String
    public var payload: String
    public var payloadLength: Int64
    public var truncated: Bool
    public var binary: Bool
    public var retained: Bool
    /// `matched`, `topic_not_mapped`, `not_a_trigger_value`, `rate_limited`, ...
    public var outcomeCode: String
    public var outcomeText: String

    public var id: Int64 { seq }
    public var matched: Bool { outcomeCode == "matched" }

    public init?(_ j: JSONValue) {
        guard let seq = j["seq"]?.int64, let code = j["outcome"]?["code"]?.string else { return nil }
        self.seq = seq
        atMs = j["at_ms"]?.int64 ?? 0
        topic = j["topic"]?.string ?? ""
        payload = j["payload"]?.string ?? ""
        payloadLength = j["payload_len"]?.int64 ?? Int64(payload.utf8.count)
        truncated = j["truncated"]?.bool ?? false
        binary = j["binary"]?.bool ?? false
        retained = j["retained"]?.bool ?? false
        outcomeCode = code
        outcomeText = j["outcome"]?["text"]?.string ?? code
    }

    public init(
        seq: Int64, atMs: Int64, topic: String, payload: String, outcomeCode: String, outcomeText: String,
        retained: Bool = false
    ) {
        self.seq = seq
        self.atMs = atMs
        self.topic = topic
        self.payload = payload
        payloadLength = Int64(payload.utf8.count)
        truncated = false
        binary = false
        self.retained = retained
        self.outcomeCode = outcomeCode
        self.outcomeText = outcomeText
    }
}

/// A page of the message log (`nv_alerts_messages`).
public struct SensorMessagePage: Sendable, Equatable {
    public var entries: [SensorMessage]
    public var nextSeq: Int64
    public var total: Int
    public var more: Bool

    public init(entries: [SensorMessage] = [], nextSeq: Int64 = 0, total: Int = 0, more: Bool = false) {
        self.entries = entries
        self.nextSeq = nextSeq
        self.total = total
        self.more = more
    }

    public static func parse(_ text: String) -> SensorMessagePage? {
        guard let v = JSONValue.parse(text), let list = v["entries"]?.array else { return nil }
        return SensorMessagePage(
            entries: list.compactMap(SensorMessage.init), nextSeq: v["next_seq"]?.int64 ?? 0,
            total: Int(v["total"]?.int64 ?? 0), more: v["more"]?.bool ?? false)
    }

    /// The `nv_alerts_messages` request.
    public static func request(input: String, after: Int64, limit: Int = 500, clear: Bool = false) -> String {
        var o: [String: Any] = ["input": input, "after": after, "limit": limit]
        if clear { o["clear"] = true }
        let data = (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// What the "Sensor messages" view shows by outcome.
public enum SensorOutcomeFilter: Hashable, Sendable, CaseIterable {
    case all, matched, ignored
    /// Ignored for this one reason (`SensorMessage.outcomeCode`).
    case code(String)

    public static var allCases: [SensorOutcomeFilter] {
        [
            .all, .matched, .ignored, .code("not_a_trigger_value"), .code("topic_not_mapped"), .code("rate_limited"),
            .code("repeat"), .code("debounced"),
        ]
    }

    public var title: String {
        switch self {
        case .all: "All messages"
        case .matched: "Matched"
        case .ignored: "Ignored"
        case .code(let c):
            switch c {
            case "not_a_trigger_value": "Not a trigger value"
            case "topic_not_mapped": "Topic not mapped"
            case "rate_limited": "Rate-limited"
            case "repeat": "Same state"
            case "debounced": "Debounced"
            default: c.replacingOccurrences(of: "_", with: " ").capitalized
            }
        }
    }

    public func accepts(_ m: SensorMessage) -> Bool {
        switch self {
        case .all: true
        case .matched: m.matched
        case .ignored: !m.matched
        case .code(let c): m.outcomeCode == c
        }
    }
}

/// The messages a view holds: pages are merged by sequence number (live
/// updating asks for what came after the last one), at most 500 kept.
public struct SensorMessageLog: Sendable, Equatable {
    public static let capacity = 500
    public private(set) var entries: [SensorMessage] = []
    /// Ask for entries from here on.
    public private(set) var nextSeq: Int64 = 0

    public init() {}

    public mutating func merge(_ page: SensorMessagePage) {
        let known = Set(entries.map(\.seq))
        entries += page.entries.filter { !known.contains($0.seq) }
        entries.sort { $0.seq < $1.seq }
        if entries.count > Self.capacity { entries.removeFirst(entries.count - Self.capacity) }
        nextSeq = max(nextSeq, page.nextSeq)
    }

    public mutating func clear() {
        entries = []
    }

    /// Newest first, as the list shows them.
    public func shown(_ filter: SensorOutcomeFilter) -> [SensorMessage] {
        entries.filter(filter.accepts).reversed()
    }
}

/// The notify targets Home Assistant offers (the `alerts_output_targets`
/// event): entities first, then legacy services, as the core sorted them.
public struct NotifyTargetList: Sendable, Equatable {
    public struct Target: Sendable, Equatable, Hashable {
        /// What a rule's target field holds (no `notify.` prefix).
        public var name: String
        public var isEntity: Bool
    }

    public var output: Int
    public var ok: Bool
    /// The reason, when `ok` is false.
    public var summary: String
    public var targets: [Target]

    public init?(json: String) {
        guard let v = JSONValue.parse(json), let output = v["output"]?.int64 else { return nil }
        self.output = Int(output)
        ok = v["ok"]?.bool ?? false
        summary = v["summary"]?.string ?? ""
        targets = (v["targets"]?.array ?? []).compactMap { t in
            guard let name = t["name"]?.string, !name.isEmpty else { return nil }
            return Target(name: name, isEntity: t["kind"]?.string == "entity")
        }
    }

    public var entities: [Target] { targets.filter(\.isEntity) }
    public var services: [Target] { targets.filter { !$0.isEntity } }
}

/// The result of "Send test alert" (the `alerts_output_test` event).
public struct OutputTestResult: Sendable, Equatable {
    public var output: Int
    public var ok: Bool
    public var summary: String
    public var httpStatus: Int?
    public var atMs: Int64

    public init?(json: String) {
        guard let v = JSONValue.parse(json), let output = v["output"]?.int64 else { return nil }
        self.output = Int(output)
        ok = v["ok"]?.bool ?? false
        summary = v["summary"]?.string ?? ""
        httpStatus = v["http_status"]?.int64.map(Int.init)
        atMs = v["at_ms"]?.int64 ?? 0
    }

    /// "Delivered (HTTP 200) at 10:32:01 AM" or "Not delivered (HTTP 404): ... at ...".
    public func line(timeZone: TimeZone = .current, locale: Locale = .current) -> String {
        let f = DateFormatter()
        f.locale = locale
        f.timeZone = timeZone
        f.timeStyle = .medium
        f.dateStyle = .none
        let time = f.string(from: Date(timeIntervalSince1970: Double(atMs) / 1000))
        let status = httpStatus.map { " (HTTP \($0))" } ?? ""
        if ok { return "Delivered\(status) at \(time)" + (summary.isEmpty ? "" : ": \(summary)") }
        return "Not delivered\(status) at \(time)" + (summary.isEmpty ? "" : ": \(summary)")
    }
}
