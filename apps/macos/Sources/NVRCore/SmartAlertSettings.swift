import Foundation

// The smart-alert settings the user edits (docs/NOTIFICATIONS.md "Settings",
// docs/UX.md "Smart alerts"), kept by the app in its own file, and the
// document the core takes (`nv_alerts_configure`, docs/FFI.md "Smart
// alerts"). Two rules shape this file:
//
// - Secrets never live here. MQTT passwords and the Home Assistant token go
//   to the Keychain only (`SmartAlertSecret` names the item, account
//   `alerts.<id>`); the saved file and its JSON carry none. The document for
//   the core gets them, in memory, in its `secrets` map (keyed by the input's
//   or output's `id`) only when the caller passes a secrets lookup.
// - Cameras are stored by the stable `CameraRef` (device key, channel, lens);
//   the core names them by this run's device IDs, so the document is built
//   at send time and cameras of devices not added now are left out. The
//   stored form (`storedDocument`, the app's `alerts.json`) names them by
//   `device_key` instead, for a host that resolves the IDs itself.

public enum AlertExposure: String, Codable, CaseIterable, Sendable {
    case outside, perimeter, inside

    public var title: String {
        switch self {
        case .outside: "Outside"
        case .perimeter: "Perimeter"
        case .inside: "Inside"
        }
    }
}

public enum AlertMode: String, Codable, CaseIterable, Sendable {
    case home, away, night

    public var title: String {
        switch self {
        case .home: "Home"
        case .away: "Away"
        case .night: "Night"
        }
    }
}

public enum AlertSeverity: String, Codable, CaseIterable, Sendable, Comparable {
    case low, normal, high, critical

    public var rank: Int {
        switch self {
        case .low: 0
        case .normal: 1
        case .high: 2
        case .critical: 3
        }
    }

    public static func < (a: AlertSeverity, b: AlertSeverity) -> Bool { a.rank < b.rank }

    public var title: String { rawValue.capitalized }
}

/// The classes a camera may alert on (the engine's `Class` names).
public enum AlertClass: String, Codable, CaseIterable, Sendable, Comparable {
    case person, vehicle, animal, package, visitor, smoke, co

    /// The classes the local detector confirms (the rest are the camera's or
    /// the sound analysis' own evidence).
    public static let detector: [AlertClass] = [.person, .vehicle, .animal]

    public var title: String {
        switch self {
        case .person: "Person"
        case .vehicle: "Vehicle"
        case .animal: "Animal"
        case .package: "Package"
        case .visitor: "Doorbell press"
        case .smoke: "Smoke alarm"
        case .co: "CO alarm"
        }
    }

    public static func < (a: AlertClass, b: AlertClass) -> Bool {
        (allCases.firstIndex(of: a) ?? 0) < (allCases.firstIndex(of: b) ?? 0)
    }
}

/// A point in normalized picture coordinates (0...1 across and down).
public struct ROIPoint: Codable, Equatable, Hashable, Sendable {
    public var x: Double
    public var y: Double

    public init(x: Double, y: Double) {
        self.x = x
        self.y = y
    }

    /// `[x, y]`, as the core's polygons are written.
    public var pair: [Double] { [x, y] }
}

/// A zone on a camera: an include zone limits alerts to itself, an exclude
/// zone (a mask) removes its area.
public struct AlertZone: Codable, Equatable, Sendable {
    public var points: [ROIPoint]
    public var include: Bool

    public init(points: [ROIPoint], include: Bool = true) {
        self.points = points
        self.include = include
    }
}

/// A daily local-time window: [start, end) in minutes, wrapping at midnight
/// when start > end; `days` bit 0 is Monday through bit 6 Sunday.
public struct AlertDailyWindow: Codable, Equatable, Sendable {
    public var days: UInt8
    public var startMin: Int
    public var endMin: Int

    public init(days: UInt8 = 0x7f, startMin: Int, endMin: Int) {
        self.days = days
        self.startMin = startMin
        self.endMin = endMin
    }
}

/// When a camera may alert (always-alert rules ignore it).
public enum AlertSchedule: Codable, Equatable, Sendable {
    case always
    case night
    case windows([AlertDailyWindow])

    private enum CodingKeys: String, CodingKey { case kind, windows }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "night": self = .night
        case "windows": self = .windows(try c.decodeIfPresent([AlertDailyWindow].self, forKey: .windows) ?? [])
        default: self = .always
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .always: try c.encode("always", forKey: .kind)
        case .night: try c.encode("night", forKey: .kind)
        case .windows(let w):
            try c.encode("windows", forKey: .kind)
            try c.encode(w, forKey: .windows)
        }
    }
}

/// One camera's smart-alert settings.
public struct AlertCameraSettings: Codable, Equatable, Sendable, Identifiable {
    public var camera: CameraRef
    public var exposure: AlertExposure = .outside
    public var entrance = false
    public var trustCameraAI = false
    public var classes: [AlertClass] = AlertClass.allCases
    public var zones: [AlertZone] = []
    public var schedule: AlertSchedule = .always
    public var verifyMotion = false

    public var id: CameraRef { camera }

    public init(camera: CameraRef) { self.camera = camera }
}

/// How an MQTT payload is read (docs/NOTIFICATIONS.md "MQTT input").
public enum MQTTPayloadFormat: String, Codable, CaseIterable, Sendable {
    /// The payload itself is compared with the on and off values.
    case plain
    /// A JSON payload; a dot path locates the value.
    case json
}

/// One camera angle that covers a sensor's area: its region of interest (no
/// points means the whole picture) and its weight in k of n.
public struct SensorCamera: Codable, Equatable, Sendable, Identifiable {
    public var camera: CameraRef
    public var roi: [ROIPoint] = []
    public var weight: Double = 1

    public var id: CameraRef { camera }

    public init(camera: CameraRef, roi: [ROIPoint] = [], weight: Double = 1) {
        self.camera = camera
        self.roi = roi
        self.weight = weight
    }
}

/// An external sensor reached through the user's MQTT broker, and the camera
/// angles that confirm it. Its password is a Keychain item
/// (`SmartAlertSecret.mqttPassword`), never a field here; the user name is a
/// plain setting (it goes into the `mqtt` input entry).
public struct MQTTSensor: Codable, Equatable, Sendable, Identifiable {
    /// The source key (`[a-z0-9_-]`, at most 32 bytes): the trigger's
    /// origin and the Keychain items' name.
    public var key: String
    public var name: String
    /// The trigger kind the sensor's triggers carry.
    public var triggerKind = "sensor_motion"
    /// The broker login's user name (nil or empty: no login).
    public var username: String?
    public var host = ""
    public var port = 1883
    public var tls = false
    /// The broker's CA as PEM; empty uses the built-in public roots.
    public var caPEM = ""
    public var topic = ""
    public var allowWildcards = false
    public var allowPublic = false
    public var payload: MQTTPayloadFormat = .plain
    public var jsonPath = ""
    public var onValues = ["ON", "true", "1"]
    public var offValues = ["OFF", "false", "0"]
    /// Every "on" is one pulse, rather than on starts and off stops.
    public var pulse = false
    /// Retained messages: `ignore` (default), `seed`, or `accept`.
    public var retained = "ignore"
    public var debounceMs = 500
    public var cameras: [SensorCamera] = []
    public var windowBeforeMs = 2000
    public var windowAfterMs = 8000
    public var windowMaxMs = 30000
    public var classes: [AlertClass] = AlertClass.detector
    /// Off only by the user's explicit choice: the sensor alone may alert
    /// (marked unverified).
    public var requireVerified = true
    public var k = 2.0
    public var minScore = 0.5
    /// When no picture could be checked: alert unverified instead of the
    /// quiet log.
    public var alertWhenUnavailable = false

    public var id: String { key }

    public init(key: String, name: String) {
        self.key = key
        self.name = name
    }

    /// A free key `sensor-N` among `taken`.
    public static func newKey(taken: [String]) -> String {
        var n = taken.count + 1
        while taken.contains("sensor-\(n)") { n += 1 }
        return "sensor-\(n)"
    }

    public static func validKey(_ key: String) -> Bool {
        !key.isEmpty && key.utf8.count <= 32
            && key.unicodeScalars.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "-" || $0 == "_" }
    }
}

/// What a Home Assistant rule does (crates/ha-output `Action`).
public enum HAActionKind: String, Codable, CaseIterable, Sendable {
    case pulse, turnOn = "turn_on", script, notify

    public var title: String {
        switch self {
        case .pulse: "Pulse (on, then off)"
        case .turnOn: "Turn on"
        case .script: "Run script"
        case .notify: "Notify"
        }
    }
}

/// One mapping rule: empty lists match anything.
public struct HARule: Codable, Equatable, Sendable, Identifiable {
    public var id: String
    public var classes: [AlertClass] = []
    /// Sensor keys.
    public var sources: [String] = []
    public var cameras: [CameraRef] = []
    public var minSeverity: AlertSeverity?
    public var action: HAActionKind = .pulse
    /// An entity ID (`input_boolean.example_alert`), or a notify service name.
    public var target = ""
    /// Overrides the output's cooldown for this rule, in seconds.
    public var cooldownSeconds: Int?

    public init(id: String = UUID().uuidString.lowercased(), target: String = "") {
        self.id = id
        self.target = target
    }
}

/// The Home Assistant output. The long-lived access token is a Keychain item
/// (`SmartAlertSecret.homeAssistantToken`), never a field here.
public struct HomeAssistantOutput: Codable, Equatable, Sendable {
    public var enabled = false
    public var baseURL = ""
    public var pictures = true
    public var minSeverity: AlertSeverity = .normal
    /// Every matching rule fires, rather than the first.
    public var allMatches = false
    public var rules: [HARule] = []
    public var pulseGapMs = 2000
    public var cooldownMs = 30000

    public init() {}
}

/// Desktop notifications (the app's own output).
public struct DesktopOutput: Codable, Equatable, Sendable {
    public var enabled = true
    public var minSeverity: AlertSeverity = .normal
    public var pictures = true

    public init() {}
}

/// Where a live check is shown (docs/NOTIFICATIONS.md "The live view").
public enum VerifyPlacementSetting: String, Codable, CaseIterable, Sendable {
    /// In the camera's tile when it is on screen, else an inset in the
    /// viewer window (the default).
    case tileThenInset
    /// In the tile when on screen, else a small window that never takes
    /// focus.
    case tileThenWindow
    /// Always the small window.
    case window

    public var title: String {
        switch self {
        case .tileThenInset: "In the tile, else an inset"
        case .tileThenWindow: "In the tile, else a small window"
        case .window: "Always in a small window"
        }
    }
}

public struct VerifyViewSettings: Codable, Equatable, Sendable {
    public var placement: VerifyPlacementSetting = .tileThenInset
    /// Also keep checks that end quiet or suppressed on screen (default:
    /// alerts and always-alert candidates only).
    public var showQuiet = false
    /// Seconds a check stays after its decision (at least 5; default 60).
    public var minSeconds = 60

    public init() {}

    public static let minimumSeconds = 5
    /// The "Stay after the decision" choices (seconds), with a stored value
    /// that is not one of them kept as its own choice.
    public static func stayChoices(including current: Int) -> [Int] {
        let base = [5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 600]
        return current >= minimumSeconds && !base.contains(current) ? (base + [current]).sorted() : base
    }

    /// "5 s", "1 min", "1 min 30 s".
    public static func stayTitle(_ seconds: Int) -> String {
        if seconds < 60 { return "\(seconds) s" }
        let m = seconds / 60, r = seconds % 60
        return r == 0 ? "\(m) min" : "\(m) min \(r) s"
    }
}

public struct AlertLocation: Codable, Equatable, Sendable {
    public var lat: Double
    public var lon: Double

    /// Rounded to 0.1° (about 11 km), latitude clamped, longitude wrapped.
    public init(lat: Double, lon: Double) {
        let la = lat.isFinite ? min(max(lat, -90), 90) : 0
        var lo = lon.isFinite ? lon : 0
        lo = (lo + 180).truncatingRemainder(dividingBy: 360)
        if lo < 0 { lo += 360 }
        lo -= 180
        self.lat = Self.tenth(la)
        self.lon = Self.tenth(lo)
    }

    static func tenth(_ v: Double) -> Double {
        let r = (v * 10).rounded() / 10
        return r == 0 ? 0 : r  // no "-0"
    }

    private enum CodingKeys: String, CodingKey { case lat, lon }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(lat: try c.decode(Double.self, forKey: .lat), lon: try c.decode(Double.self, forKey: .lon))
    }
}

public struct AlertNightSettings: Codable, Equatable, Sendable {
    public var afterSunsetMin = 30
    public var beforeSunriseMin = 30
    /// Night hours without a location (local minutes).
    public var fallbackStartMin = 22 * 60
    public var fallbackEndMin = 6 * 60

    public init() {}
}

public struct AlwaysAlertSettings: Codable, Equatable, Sendable {
    public var smokeCO = true
    public var nightOutsidePerson = true
    public var doorbellNight = true

    public init() {}
}

/// Checks of a camera's own detections (advanced).
public struct AlertVerifySettings: Codable, Equatable, Sendable {
    public var k = 2
    public var minScore = 0.5
    public var windowBeforeS = 2
    public var windowAfterS = 10

    public init() {}
}

/// A Keychain item of the smart-alert settings.
public enum SmartAlertSecret: Hashable, Sendable {
    /// A sensor's broker password (by the sensor's key).
    case mqttPassword(String)
    case homeAssistantToken

    /// The `id` of the input or output entry it belongs to: its key in the
    /// document's `secrets` map (docs/FFI.md "Smart alerts").
    public var id: String {
        switch self {
        case .mqttPassword(let key): key
        case .homeAssistantToken: SmartAlertSettings.homeAssistantID
        }
    }

    /// The Keychain account name: `alerts.<id>`.
    public var account: String { "alerts.\(id)" }
}

/// Everything on Settings > Smart Alerts.
public struct SmartAlertSettings: Codable, Equatable, Sendable {
    /// Off until the user turns smart alerts on.
    public var enabled = false
    public var mode: AlertMode = .home
    public var location: AlertLocation?
    public var night = AlertNightSettings()
    public var alwaysAlert = AlwaysAlertSettings()
    public var minAlertSeverity: AlertSeverity = .normal
    public var verification = AlertVerifySettings()
    public var cameras: [AlertCameraSettings] = []
    public var sensors: [MQTTSensor] = []
    public var desktop = DesktopOutput()
    public var homeAssistant = HomeAssistantOutput()
    public var view = VerifyViewSettings()
    /// Switch to standby automatically on battery (offered; off).
    public var standbyOnBattery = false
    /// How long the frames and detections of a check are kept for review
    /// (30 minutes to 24 hours).
    public var evidenceRetentionMinutes = 120
    /// How long after a night ends the pictures made during it are kept, in
    /// hours (0 to 12), so the night can be reviewed the next morning.
    public var evidenceNightHoldHours = 4

    public init() {}

    private enum CodingKeys: String, CodingKey {
        case enabled, mode, location, night, alwaysAlert, minAlertSeverity, verification, cameras, sensors, desktop,
            homeAssistant, view, standbyOnBattery, evidenceRetentionMinutes, evidenceNightHoldHours
    }

    /// Every field is optional in the file: a missing or unreadable part
    /// keeps its default rather than losing the whole document.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func get<T: Decodable>(_ t: T.Type, _ k: CodingKeys) -> T? { (try? c.decodeIfPresent(t, forKey: k)) ?? nil }
        let d = SmartAlertSettings()
        enabled = get(Bool.self, .enabled) ?? d.enabled
        mode = get(AlertMode.self, .mode) ?? d.mode
        location = get(AlertLocation.self, .location)
        night = get(AlertNightSettings.self, .night) ?? d.night
        alwaysAlert = get(AlwaysAlertSettings.self, .alwaysAlert) ?? d.alwaysAlert
        minAlertSeverity = get(AlertSeverity.self, .minAlertSeverity) ?? d.minAlertSeverity
        verification = get(AlertVerifySettings.self, .verification) ?? d.verification
        cameras = get([AlertCameraSettings].self, .cameras) ?? []
        sensors = get([MQTTSensor].self, .sensors) ?? []
        desktop = get(DesktopOutput.self, .desktop) ?? d.desktop
        homeAssistant = get(HomeAssistantOutput.self, .homeAssistant) ?? d.homeAssistant
        view = get(VerifyViewSettings.self, .view) ?? d.view
        standbyOnBattery = get(Bool.self, .standbyOnBattery) ?? d.standbyOnBattery
        evidenceRetentionMinutes = min(max(get(Int.self, .evidenceRetentionMinutes) ?? d.evidenceRetentionMinutes, 30), 1440)
        evidenceNightHoldHours = min(max(get(Int.self, .evidenceNightHoldHours) ?? d.evidenceNightHoldHours, 0), 12)
    }

    // MARK: Editing helpers

    /// A camera's settings, or the defaults for one not listed.
    public func camera(_ ref: CameraRef) -> AlertCameraSettings {
        cameras.first { $0.camera == ref } ?? AlertCameraSettings(camera: ref)
    }

    /// Replaces (or adds) one camera's settings.
    public mutating func setCamera(_ s: AlertCameraSettings) {
        if let i = cameras.firstIndex(where: { $0.camera == s.camera }) { cameras[i] = s } else { cameras.append(s) }
    }

    /// The Keychain items these settings use (to delete those of removed
    /// sensors).
    public var secrets: Set<SmartAlertSecret> {
        var s: Set<SmartAlertSecret> = [.homeAssistantToken]
        for sensor in sensors { s.insert(.mqttPassword(sensor.key)) }
        return s
    }

    /// The file's bytes (sorted keys, no secret in it by construction).
    public func fileData() -> Data {
        let e = JSONEncoder()
        e.outputFormatting = [.prettyPrinted, .sortedKeys]
        return (try? e.encode(self)) ?? Data("{}".utf8)
    }

    public static func fromFile(_ data: Data) -> SmartAlertSettings {
        (try? JSONDecoder().decode(SmartAlertSettings.self, from: data)) ?? SmartAlertSettings()
    }

    // MARK: Problems shown in Settings

    /// What would keep a part from working, in words for the settings
    /// screen. The core clamps rather than refuses; these are the things
    /// clamping cannot fix.
    public func problems() -> [String] {
        var out: [String] = []
        var keys: Set<String> = []
        for (i, s) in sensors.enumerated() {
            let label = s.name.isEmpty ? "Sensor \(i + 1)" : s.name
            if !MQTTSensor.validKey(s.key) { out.append("\(label): the key may only use a-z, 0-9, - and _ (at most 32).") }
            if !keys.insert(s.key).inserted { out.append("\(label): another sensor has the same key.") }
            if s.host.trimmingCharacters(in: .whitespaces).isEmpty { out.append("\(label): no broker address.") }
            if !(1...65535).contains(s.port) { out.append("\(label): the port must be 1 to 65535.") }
            if s.topic.isEmpty { out.append("\(label): no topic.") }
            if !s.allowWildcards, s.topic.contains("+") || s.topic.contains("#") {
                out.append("\(label): the topic has wildcards; allow them or use one exact topic.")
            }
            if s.payload == .json, s.jsonPath.trimmingCharacters(in: .whitespaces).isEmpty {
                out.append("\(label): a JSON payload needs a path to the value.")
            }
            if s.requireVerified, s.cameras.isEmpty {
                out.append("\(label): choose at least one camera that sees the sensor's area.")
            }
            if !s.allowPublic, !AlertEndpoint.hostIsLocal(s.host), !s.host.isEmpty {
                out.append("\(label): the broker may not be on the local network; it will be refused unless allowed.")
            }
        }
        let ha = homeAssistant
        if ha.enabled {
            switch AlertEndpoint.checkHomeAssistantURL(ha.baseURL) {
            case .some(let why): out.append("Home Assistant: \(why)")
            case .none: break
            }
            for (i, r) in ha.rules.enumerated() {
                if let why = HARuleCheck.problem(r) { out.append("Home Assistant rule \(i + 1): \(why)") }
            }
        }
        return out
    }
}

/// Checks a Home Assistant rule's target before it reaches the output (which
/// refuses the same things).
public enum HARuleCheck {
    public static func problem(_ r: HARule) -> String? {
        let t = r.target.trimmingCharacters(in: .whitespaces)
        func entity(_ allowed: [String]) -> String? {
            let parts = t.split(separator: ".", omittingEmptySubsequences: false)
            guard parts.count == 2, parts.allSatisfy({ !$0.isEmpty && $0.allSatisfy(Self.identChar) }) else {
                return "the entity ID must look like domain.name (for example input_boolean.example_alert)."
            }
            return allowed.contains(String(parts[0])) ? nil : "this action needs a \(allowed.joined(separator: " or ")) entity."
        }
        switch r.action {
        case .pulse, .turnOn: return entity(["input_boolean", "switch"])
        case .script: return entity(["script"])
        case .notify:
            // A pasted `notify.example_phone` is fine; the core drops the prefix.
            let name = t.hasPrefix("notify.") ? String(t.dropFirst("notify.".count)) : t
            return !name.isEmpty && name.allSatisfy(Self.identChar) ? nil : "the notify target is lowercase letters, digits and _ (a pasted notify.name is fine)."
        }
    }

    static func identChar(_ c: Character) -> Bool { c.isASCII && (c.isLowercase || c.isNumber || c == "_") }
}

/// Endpoint checks (the same heuristics as the core: crates/alerts
/// `endpoint_may_leave_network`).
public enum AlertEndpoint {
    /// The host of a URL (or of a bare host[:port]); lowercased, brackets of
    /// an IPv6 address removed.
    public static func host(of endpoint: String) -> String {
        let rest = endpoint.range(of: "://").map { String(endpoint[$0.upperBound...]) } ?? endpoint
        let authority = rest.split(whereSeparator: { "/?#".contains($0) }).first.map(String.init) ?? ""
        let hostPort = authority.split(separator: "@", omittingEmptySubsequences: false).last.map(String.init) ?? ""
        if hostPort.hasPrefix("[") {
            return String(hostPort.dropFirst().split(separator: "]").first ?? "").lowercased()
        }
        // A bare IPv6 address (two or more colons) has no port.
        if hostPort.filter({ $0 == ":" }).count > 1 { return hostPort.lowercased() }
        return String(hostPort.split(separator: ":", omittingEmptySubsequences: false).first ?? "").lowercased()
    }

    /// Whether a host is on the local network: loopback, private, link-local
    /// or carrier-grade NAT addresses, `localhost`, `.local`, `.lan`,
    /// `.home.arpa`, `.internal` names, and single-label names.
    public static func hostIsLocal(_ rawHost: String) -> Bool {
        let host = rawHost.lowercased()
        guard !host.isEmpty else { return false }
        if let v4 = ipv4(host) {
            let (a, b) = (v4[0], v4[1])
            return a == 127 || a == 10 || (a == 172 && (16...31).contains(b)) || (a == 192 && b == 168)
                || (a == 169 && b == 254) || (a == 100 && (64...127).contains(b))
        }
        if host.contains(":") {
            let first = host.split(separator: ":", omittingEmptySubsequences: false).first.map(String.init) ?? ""
            if host == "::1" { return true }
            guard let seg = UInt16(first.isEmpty ? "0" : first, radix: 16) else { return false }
            return (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
        }
        let suffixes = [".local", ".lan", ".home.arpa", ".internal"]
        return host == "localhost" || !host.contains(".") || suffixes.contains { host.hasSuffix($0) }
    }

    /// Whether pictures sent to this endpoint may leave the local network
    /// (the warning beside an output's "Pictures" switch).
    public static func mayLeaveNetwork(_ endpoint: String) -> Bool { !hostIsLocal(host(of: endpoint)) }

    /// Why a Home Assistant address cannot be used, or nil: http or https,
    /// a host, no user name or password, no query; plain http only on the
    /// local network.
    public static func checkHomeAssistantURL(_ text: String) -> String? {
        let t = text.trimmingCharacters(in: .whitespaces)
        guard let r = t.range(of: "://") else { return "the address must start with http:// or https://." }
        let scheme = t[..<r.lowerBound].lowercased()
        guard scheme == "http" || scheme == "https" else { return "only http:// and https:// addresses." }
        if t.contains("?") || t.contains("#") { return "no query or fragment in the address." }
        let authority = t[r.upperBound...].split(separator: "/").first.map(String.init) ?? ""
        if authority.contains("@") { return "no user name or password in the address." }
        let h = host(of: t)
        if h.isEmpty { return "the address has no host." }
        if scheme == "http", !hostIsLocal(h) { return "http:// is only for hosts on the local network; use https://." }
        return nil
    }

    private static func ipv4(_ s: String) -> [Int]? {
        let parts = s.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 4 else { return nil }
        let n = parts.compactMap { p -> Int? in
            guard !p.isEmpty, p.count <= 3, p.allSatisfy(\.isNumber), let v = Int(p), v <= 255 else { return nil }
            return v
        }
        return n.count == 4 ? n : nil
    }
}

// MARK: The core's document

extension SmartAlertSettings {
    /// How the document names things at send time.
    public struct CoreContext {
        /// This run's device ID for a device key, nil when it is not added.
        public var deviceID: (String) -> UInt64?
        /// The user's own name for a camera (shown in explanations).
        public var name: (CameraRef) -> String?
        /// The computer's current offset from UTC, in minutes.
        public var utcOffsetMinutes: Int
        /// The Keychain lookup; nil leaves every secret out.
        public var secret: ((SmartAlertSecret) -> String?)?
        /// Every camera known now: those without settings of their own are
        /// sent with the defaults (so explanations carry their names).
        public var allCameras: [CameraRef]
        /// The entrance mark the alert insets use (Settings > Cameras), when
        /// the app keeps it; else the settings' own.
        public var entrance: ((CameraRef) -> Bool)?
        /// Name cameras by `device_key` (the stored form) instead of this
        /// run's device IDs; no camera is left out.
        public var byDeviceKey = false

        public init(
            deviceID: @escaping (String) -> UInt64?, name: @escaping (CameraRef) -> String? = { _ in nil },
            utcOffsetMinutes: Int, secret: ((SmartAlertSecret) -> String?)? = nil, allCameras: [CameraRef] = [],
            entrance: ((CameraRef) -> Bool)? = nil
        ) {
            self.deviceID = deviceID
            self.name = name
            self.utcOffsetMinutes = utcOffsetMinutes
            self.secret = secret
            self.allCameras = allCameras
            self.entrance = entrance
        }
    }

    /// `{"device","channel","lens"}` with this run's device ID (or
    /// `device_key` in the stored form).
    static func camera(_ ref: CameraRef, _ ctx: CoreContext) -> [String: Any]? {
        guard ref.channel >= 0, ref.lens >= 0 else { return nil }
        if ctx.byDeviceKey { return ["device_key": ref.device, "channel": ref.channel, "lens": ref.lens] }
        guard let id = ctx.deviceID(ref.device) else { return nil }
        return ["device": id, "channel": ref.channel, "lens": ref.lens]
    }

    static func polygon(_ points: [ROIPoint]) -> [[Double]] { points.map(\.pair) }

    /// The document for `nv_alerts_configure` (docs/FFI.md "Smart alerts").
    /// Disabled settings send only `{"enabled":false}` so the core lets go
    /// of everything.
    public func coreDocument(_ ctx: CoreContext) -> [String: Any] {
        guard enabled else { return ["enabled": false, "utc_offset_min": ctx.utcOffsetMinutes] }
        var doc: [String: Any] = [
            "version": 1, "enabled": true, "utc_offset_min": ctx.utcOffsetMinutes, "mode": mode.rawValue,
            "min_alert_severity": minAlertSeverity.rawValue,
            "always_alert": [
                "smoke_co": alwaysAlert.smokeCO, "night_outside_person": alwaysAlert.nightOutsidePerson,
                "doorbell_night": alwaysAlert.doorbellNight,
            ],
            "night": [
                "after_sunset_min": night.afterSunsetMin, "before_sunrise_min": night.beforeSunriseMin,
                "fallback_start_min": night.fallbackStartMin, "fallback_end_min": night.fallbackEndMin,
            ],
            "verification": [
                "k": verification.k, "min_score": verification.minScore,
                "window_before_s": verification.windowBeforeS, "window_after_s": verification.windowAfterS,
            ],
            "standby": ["auto_on_battery": standbyOnBattery, "max_wake_per_device": 4],
            "evidence": [
                "retention_min": min(max(evidenceRetentionMinutes, 30), 1440),
                "night_hold_h": min(max(evidenceNightHoldHours, 0), 12),
            ],
        ]
        doc["location"] = location.map { ["lat": $0.lat, "lon": $0.lon] } ?? NSNull()
        let listed = Set(cameras.map(\.camera))
        let all = cameras + ctx.allCameras.filter { !listed.contains($0) }.map(AlertCameraSettings.init(camera:))
        doc["cameras"] = all.compactMap { c -> [String: Any]? in
            guard let cam = Self.camera(c.camera, ctx) else { return nil }
            var o: [String: Any] = [
                "camera": cam, "exposure": c.exposure.rawValue, "entrance": ctx.entrance?(c.camera) ?? c.entrance,
                "trust_camera_ai": c.trustCameraAI, "classes": c.classes.sorted().map(\.rawValue),
                "zones": c.zones.filter { $0.points.count >= 3 }.map {
                    ["polygon": Self.polygon($0.points), "include": $0.include] as [String: Any]
                },
                "verify_motion": c.verifyMotion,
            ]
            switch c.schedule {
            case .always: o["schedule"] = ["kind": "always"]
            case .night: o["schedule"] = ["kind": "night"]
            case .windows(let w):
                o["schedule"] = [
                    "kind": "windows",
                    "windows": w.map { ["days": Int($0.days), "start_min": $0.startMin, "end_min": $0.endMin] },
                ]
            }
            if let n = ctx.name(c.camera), !n.isEmpty { o["name"] = n }
            return o
        }
        doc["sources"] = sensors.map { s -> [String: Any] in
            var o: [String: Any] = [
                "key": s.key,
                "cameras": s.cameras.compactMap { sc -> [String: Any]? in
                    guard let cam = Self.camera(sc.camera, ctx) else { return nil }
                    return ["camera": cam, "roi": Self.polygon(sc.roi.count >= 3 ? sc.roi : []), "weight": sc.weight]
                },
                "window_before_ms": s.windowBeforeMs, "window_after_ms": s.windowAfterMs,
                "window_max_ms": s.windowMaxMs, "classes": s.classes.sorted().map(\.rawValue),
                "require": s.requireVerified ? "verified" : "none", "k": s.k, "min_score": s.minScore,
                "on_unavailable": s.alertWhenUnavailable ? "alert_unverified" : "quiet",
            ]
            if !s.name.isEmpty { o["name"] = s.name }
            return o
        }
        var secrets: [String: String] = [:]
        // The `mqtt` input module's entry: the `mqtt-triggers` source
        // settings, flat, with `trigger_kind` and `username` beside them.
        doc["inputs"] = sensors.map { s -> [String: Any] in
            var broker: [String: Any] = ["host": s.host.trimmingCharacters(in: .whitespaces), "port": s.port]
            broker["tls"] = s.tls ? (s.caPEM.isEmpty ? [:] : ["ca_pem": s.caPEM]) as [String: Any] : NSNull()
            var mapping: [String: Any] = [
                "on_values": s.onValues, "off_values": s.offValues, "retained": s.retained,
                "mode": s.pulse ? "pulse" : "level", "debounce_ms": s.debounceMs,
            ]
            mapping["json_path"] = s.payload == .json ? s.jsonPath : NSNull()
            var o: [String: Any] = [
                "kind": "mqtt", "id": s.key, "trigger_kind": s.triggerKind, "broker": broker, "topic": s.topic,
                "allow_wildcards": s.allowWildcards, "allow_public": s.allowPublic, "mapping": mapping,
            ]
            if let u = s.username?.trimmingCharacters(in: .whitespaces), !u.isEmpty { o["username"] = u }
            if let p = ctx.secret?(.mqttPassword(s.key)), !p.isEmpty { secrets[s.key] = p }
            return o
        }
        var outputs: [[String: Any]] = [
            [
                "kind": "desktop", "enabled": desktop.enabled, "min_severity": desktop.minSeverity.rawValue,
                "pictures": desktop.pictures,
            ]
        ]
        let ha = homeAssistant
        if ha.enabled, !ha.baseURL.isEmpty {
            // The `home_assistant` output module's entry: the common output
            // fields plus the `ha-output` settings, flat.
            outputs.append([
                "kind": "home_assistant", "id": Self.homeAssistantID, "enabled": true, "endpoint": ha.baseURL,
                "min_severity": ha.minSeverity.rawValue, "pictures": ha.pictures,
                "mode": ha.allMatches ? "all_matches" : "first_match", "pulse_gap_ms": ha.pulseGapMs,
                "cooldown_ms": ha.cooldownMs, "rules": ha.rules.map { Self.haRule($0, ctx) },
            ])
            if let token = ctx.secret?(.homeAssistantToken), !token.isEmpty { secrets[Self.homeAssistantID] = token }
        }
        doc["outputs"] = outputs
        if !secrets.isEmpty { doc["secrets"] = secrets }
        return doc
    }

    static func haRule(_ r: HARule, _ ctx: CoreContext) -> [String: Any] {
        let target = r.target.trimmingCharacters(in: .whitespaces)
        let action: [String: Any] =
            r.action == .notify
            ? ["kind": "notify", "service": target] : ["kind": r.action.rawValue, "entity_id": target]
        var o: [String: Any] = [
            "classes": r.classes.sorted().map(\.rawValue), "sources": r.sources,
            "cameras": r.cameras.compactMap { camera($0, ctx) }, "action": action,
        ]
        if let s = r.minSeverity { o["min_severity"] = s.rawValue }
        if let c = r.cooldownSeconds { o["cooldown_ms"] = c * 1000 }
        return o
    }

    /// The `id` of the Home Assistant output entry (its secret's key).
    public static let homeAssistantID = "home_assistant"

    /// The stored form of the document (the app's `alerts.json`): cameras
    /// by `device_key`, every camera kept, no `utc_offset_min`, no secrets.
    /// A host that resolves device keys and adds the offset and the
    /// Keychain's secrets at send time can configure the core from it.
    public func storedDocument(
        name: @escaping (CameraRef) -> String? = { _ in nil }, allCameras: [CameraRef] = [],
        entrance: ((CameraRef) -> Bool)? = nil
    ) -> [String: Any] {
        var ctx = CoreContext(
            deviceID: { _ in nil }, name: name, utcOffsetMinutes: 0, allCameras: allCameras, entrance: entrance)
        ctx.byDeviceKey = true
        var doc = coreDocument(ctx)
        doc["utc_offset_min"] = nil
        return doc
    }

    /// The stored form as JSON bytes (sorted keys, pretty).
    public func storedJSON(
        name: @escaping (CameraRef) -> String? = { _ in nil }, allCameras: [CameraRef] = [],
        entrance: ((CameraRef) -> Bool)? = nil
    ) -> Data {
        (try? JSONSerialization.data(
            withJSONObject: storedDocument(name: name, allCameras: allCameras, entrance: entrance),
            options: [.sortedKeys, .prettyPrinted, .withoutEscapingSlashes])) ?? Data("{}".utf8)
    }

    /// The document as JSON text (sorted keys).
    public func coreJSON(_ ctx: CoreContext) -> String {
        let data =
            (try? JSONSerialization.data(
                withJSONObject: coreDocument(ctx), options: [.sortedKeys, .withoutEscapingSlashes])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}
