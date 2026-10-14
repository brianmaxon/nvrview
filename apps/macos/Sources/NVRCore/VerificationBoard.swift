import Foundation

// The live verification view's model (docs/NOTIFICATIONS.md "The live view",
// docs/UX.md "Smart alerts"): every check the core runs, built from its
// `verify_step` events, its pictures' boxes as the app's detector answered
// them, its decision and the alert's end; how long each stays on screen;
// and the words of its step log. Pure and headless: times are the viewer
// clock's seconds, passed in.

extension JSONValue {
    public subscript(key: String) -> JSONValue? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    public var string: String? { if case .string(let s) = self { s } else { nil } }
    public var bool: Bool? { if case .bool(let b) = self { b } else { nil } }
    public var array: [JSONValue]? { if case .array(let a) = self { a } else { nil } }

    public var double: Double? {
        switch self {
        case .integer(let i): Double(i)
        case .number(let d): d
        default: nil
        }
    }

    public var int64: Int64? {
        switch self {
        case .integer(let i): i
        case .number(let d) where d.isFinite: Int64(d)
        default: nil
        }
    }

    public static func parse(_ data: Data) -> JSONValue? { try? JSONDecoder().decode(JSONValue.self, from: data) }
    public static func parse(_ text: String) -> JSONValue? { parse(Data(text.utf8)) }

    /// A camera object (`{"device","channel","lens"}`).
    public var alertCamera: AlertCamera? {
        guard let d = self["device"]?.int64, let c = self["channel"]?.int64, d >= 0, (0...Int64(UInt16.max)).contains(c)
        else { return nil }
        return AlertCamera(device: UInt64(d), channel: UInt16(c), lens: UInt8(clamping: self["lens"]?.int64 ?? 0))
    }
}

/// One step of a check's trace.
public struct TraceStep: Sendable, Equatable {
    public var step: String
    public var atMs: Int64
    public var fields: JSONValue

    public init?(_ json: JSONValue) {
        guard let s = json["step"]?.string else { return nil }
        step = s
        atMs = json["at_ms"]?.int64 ?? 0
        fields = json
    }

    public var camera: AlertCamera? { fields["camera"]?.alertCamera }
}

/// How a box the detector found counts (drawn differently and labeled, not
/// by color alone).
public enum BoxVerdict: String, Sendable, Equatable {
    /// An allowed class at or above the floor, its foot in the region.
    case hit
    case belowFloor
    case outsideRegion
    /// A class this check does not look for.
    case otherClass

    public var label: String {
        switch self {
        case .hit: "hit"
        case .belowFloor: "below floor"
        case .outsideRegion: "outside region"
        case .otherClass: "not looked for"
        }
    }
}

/// A box drawn over a picture of a check.
public struct VerifyBox: Sendable, Equatable {
    public var label: String
    /// The alert class the label counts as (`car` is `vehicle`).
    public var alertClass: String
    public var score: Double
    public var x, y, w, h: Double
    public var verdict: BoxVerdict

    /// The text on the box: class, score, and why it does not count.
    public var caption: String {
        let s = String(format: "%@ %.2f", alertClass, score)
        return verdict == .hit ? s : "\(s) · \(verdict.label)"
    }

    /// The alert class of a detector label (the core's mapping).
    public static func alertClass(of label: String) -> String {
        switch label.lowercased() {
        case "car", "truck", "bus", "motorcycle": "vehicle"
        case "dog", "cat": "animal"
        case let other: other
        }
    }

    /// Judges a detection: allowed class, score floor, foot point in the
    /// region (an empty region is the whole picture).
    public static func judge(
        _ d: AlertDetection, classes: [String], minScore: Double, region: [ROIPoint]
    ) -> VerifyBox {
        let cls = alertClass(of: d.label)
        let verdict: BoxVerdict
        if !classes.isEmpty, !classes.contains(cls) {
            verdict = .otherClass
        } else if d.score < minScore {
            verdict = .belowFloor
        } else if region.count >= 3, !ROIGeometry.contains(region, ROIGeometry.foot(x: d.x, y: d.y, w: d.w, h: d.h)) {
            verdict = .outsideRegion
        } else {
            verdict = .hit
        }
        return VerifyBox(label: d.label, alertClass: cls, score: d.score, x: d.x, y: d.y, w: d.w, h: d.h, verdict: verdict)
    }
}

/// The boxes of the latest picture checked on one angle.
public struct VerifyPicture: Sendable, Equatable {
    public var request: String
    public var boxes: [VerifyBox]
    /// When the answer came (viewer clock).
    public var at: Double
}

/// One check, from its first step to its end.
public struct VerifyCheck: Sendable, Equatable, Identifiable {
    /// The trigger's ID.
    public var id: UInt64
    public var steps: [TraceStep] = []
    /// The angles it looks at, in order of appearance.
    public var cameras: [AlertCamera] = []
    /// The source's index in the settings' sensors (external triggers).
    public var sensor: Int?
    public var source: String?
    public var kind: String?
    public var className: String?
    public var classes: [String] = []
    public var minScore = 0.5
    public var pictures: [AlertCamera: VerifyPicture] = [:]
    /// The gate let it through as an always-alert candidate.
    public var candidate = false
    /// `alert`, `quiet`, or `suppress` once decided.
    public var decision: String?
    public var severity: String?
    /// Why an alert was logged but did not notify (`cooldown`).
    public var quieted: String?
    public var reason: String?
    public var explanation: String?
    public var alertID: UInt64?
    public var startedAt: Double
    public var lastStepAt: Double
    public var decidedAt: Double?
    public var settledAt: Double?
    public var usedAt: Double
    public var hovered = false

    public init(id: UInt64, now: Double) {
        self.id = id
        startedAt = now
        lastStepAt = now
        usedAt = now
    }

    /// The first camera (the one an inset or a tile band belongs to).
    public var primaryCamera: AlertCamera? { cameras.first }

    public var isAlert: Bool { decision == "alert" }

    /// One line for a header: what, where, and how it stands.
    public func headline(cameraName: (AlertCamera) -> String) -> String {
        let what = (className ?? kind).map { $0 == "co" ? "CO" : $0.capitalized } ?? "Check"
        var parts = [what]
        if let c = primaryCamera { parts.append(cameraName(c)) } else if source == "external" { parts.append("sensor") }
        switch decision {
        case nil: parts.append("checking…")
        case "alert":
            parts.append("alert" + (severity.map { " (\($0))" } ?? "") + (quieted == nil ? "" : ", not notified"))
        case "quiet": parts.append("quiet log")
        default: parts.append("suppressed")
        }
        return parts.joined(separator: " · ")
    }
}

/// Where a check is shown.
public enum VerifyPlace: Sendable, Equatable {
    case tile, inset, window

    /// In the tile when the camera is on screen (unless the setting says a
    /// window always), else an inset or the window per the setting.
    public static func place(_ setting: VerifyPlacementSetting, onScreen: Bool) -> VerifyPlace {
        switch setting {
        case .window: .window
        case .tileThenInset: onScreen ? .tile : .inset
        case .tileThenWindow: onScreen ? .tile : .window
        }
    }
}

/// The checks shown, newest first.
public struct VerificationBoard: Sendable, Equatable {
    /// How many are drawn at once; the rest show as "+N more".
    public static let maxShown = 3
    /// The most checks kept (the oldest go first).
    public static let maxChecks = 32
    /// The most steps kept per check (the core sends at most 64 plus
    /// deliveries).
    public static let maxSteps = 96
    /// A check that got no decision this long after its last step counts as
    /// decided (a lost decision must not keep it on screen for good).
    public static let lostDecisionAfter = 120.0
    /// An alert whose end never came counts as ended this long after its
    /// decision (the core re-checks for at most 10 minutes).
    public static let lostSettleAfter = 900.0

    public private(set) var checks: [VerifyCheck] = []
    public var settings = VerifyViewSettings()

    public init(settings: VerifyViewSettings = VerifyViewSettings()) { self.settings = settings }

    public func check(_ id: UInt64) -> VerifyCheck? { checks.first { $0.id == id } }

    private mutating func with(_ id: UInt64, now: Double, create: Bool, _ body: (inout VerifyCheck) -> Void) {
        if let i = checks.firstIndex(where: { $0.id == id }) {
            body(&checks[i])
        } else if create {
            var c = VerifyCheck(id: id, now: now)
            body(&c)
            checks.insert(c, at: 0)
            if checks.count > Self.maxChecks { checks.removeLast(checks.count - Self.maxChecks) }
        }
    }

    private static func addCamera(_ c: AlertCamera?, to check: inout VerifyCheck) {
        if let c, !check.cameras.contains(c) { check.cameras.append(c) }
    }

    /// A `verify_step` event's step.
    public mutating func step(triggerID: UInt64, _ json: JSONValue, now: Double) {
        guard let s = TraceStep(json) else { return }
        with(triggerID, now: now, create: true) { c in
            Self.apply(s, to: &c)
            if c.steps.count < Self.maxSteps { c.steps.append(s) }
            c.lastStepAt = now
        }
    }

    private static func apply(_ s: TraceStep, to c: inout VerifyCheck) {
        let f = s.fields
        switch s.step {
        case "trigger_received":
            c.source = f["source"]?.string
            c.kind = f["kind"]?.string
            c.className = f["class"]?.string
            c.sensor = f["sensor"]?.int64.map(Int.init)
            addCamera(s.camera, to: &c)
        case "gate":
            if f["result"]?.string == "pass_always" { c.candidate = true }
        case "sampling":
            for cam in f["cameras"]?.array ?? [] { addCamera(cam.alertCamera, to: &c) }
            if let cl = f["classes"]?.array?.compactMap(\.string) { c.classes = cl }
        case "camera_woken", "frame_sampled":
            addCamera(s.camera, to: &c)
        default:
            break
        }
    }

    /// A `verify_request` the core sent (its picture's boxes come later).
    public mutating func request(_ r: VerifyRequest, now: Double) {
        for id in r.triggerIDs {
            with(id, now: now, create: true) { c in
                Self.addCamera(r.camera, to: &c)
                if c.classes.isEmpty { c.classes = r.classes }
                c.minScore = r.minScore
            }
        }
    }

    /// The detector's answer to a request: the boxes, judged against the
    /// check's classes, floor and the angle's region.
    public mutating func answered(
        _ r: VerifyRequest, detections: [AlertDetection], region: (VerifyCheck) -> [ROIPoint], now: Double
    ) {
        for id in r.triggerIDs {
            with(id, now: now, create: true) { c in
                Self.addCamera(r.camera, to: &c)
                let classes = c.classes.isEmpty ? r.classes : c.classes
                let reg = region(c)
                c.pictures[r.camera] = VerifyPicture(
                    request: r.request,
                    boxes: detections.map { VerifyBox.judge($0, classes: classes, minScore: r.minScore, region: reg) },
                    at: now)
            }
        }
    }

    /// An `alert` event (`new`, `update`, `logged`, or `settled`).
    public mutating func alertEvent(_ json: JSONValue, now: Double) {
        let state = json["state"]?.string
        if state == "settled" {
            guard let a = json["alert_id"]?.int64 else { return }
            for i in checks.indices where checks[i].alertID == UInt64(a) { checks[i].settledAt = now }
            return
        }
        guard let d = json["decision"], let t = d["trigger_id"]?.int64, t >= 0 else { return }
        with(UInt64(t), now: now, create: true) { c in
            c.decision = d["decision"]?.string
            c.severity = d["severity"]?.string
            c.quieted = d["quieted"]?["why"]?.string
            c.reason = d["reason"]?.string
            c.explanation = d["explanation"]?.string
            c.alertID = d["alert_id"]?.int64.map { UInt64($0) }
            c.decidedAt = now
            if c.className == nil { c.className = d["class"]?.string }
            Self.addCamera(d["camera"]?.alertCamera, to: &c)
            // The decision carries the whole trace: it fills in steps sent
            // before this view listened.
            let trace = (d["trace"]?["steps"]?.array ?? []).compactMap(TraceStep.init)
            if trace.count > c.steps.count {
                let later = c.steps.filter { $0.step == "delivery" }
                c.steps = Array((trace + later).prefix(Self.maxSteps))
                for s in trace { Self.apply(s, to: &c) }
            }
            if d["always"]?.string != nil { c.candidate = true }
        }
    }

    // MARK: What is shown

    /// Whether a check is shown at all: pending checks and alerts always;
    /// quiet and suppressed ones only with "show quiet checks" or when they
    /// were always-alert candidates.
    public func isShown(_ c: VerifyCheck) -> Bool {
        switch c.decision {
        case nil, "alert": true
        default: settings.showQuiet || c.candidate
        }
    }

    /// The checks shown, newest first.
    public var shown: [VerifyCheck] { checks.filter(isShown) }
    /// The ones drawn.
    public var visible: [VerifyCheck] { Array(shown.prefix(Self.maxShown)) }
    /// How many more there are than drawn.
    public var more: Int { max(shown.count - Self.maxShown, 0) }

    /// When a check goes: its minimum time after it ended (decided, or for
    /// an alert, settled) and after it was last used; never while hovered or
    /// still pending.
    public func closesAt(_ c: VerifyCheck) -> Double? {
        if c.hovered { return nil }
        let minimum = Double(max(settings.minSeconds, VerifyViewSettings.minimumSeconds))
        let ended: Double
        if let decided = c.decidedAt {
            if c.isAlert {
                ended = c.settledAt ?? (decided + Self.lostSettleAfter)
            } else {
                ended = decided
            }
        } else {
            ended = c.lastStepAt + Self.lostDecisionAfter
        }
        return max(ended, c.usedAt) + minimum
    }

    /// The next time `tick` may close something.
    public var deadline: Double? { checks.compactMap(closesAt).min() }

    /// Removes checks whose time is up (and quiet ones that are not shown,
    /// once decided). Returns the IDs removed.
    @discardableResult
    public mutating func tick(now: Double) -> [UInt64] {
        let board = self
        let gone = Set(
            checks.filter { c in
                (c.decision != nil && !board.isShown(c)) || (board.closesAt(c).map { now >= $0 } ?? false)
            }.map(\.id))
        checks.removeAll { gone.contains($0.id) }
        return gone.sorted()
    }

    public mutating func dismiss(_ id: UInt64) { checks.removeAll { $0.id == id } }

    /// "Dismiss All": every check shown goes.
    public mutating func dismissAll() {
        let board = self
        checks.removeAll(where: board.isShown)
    }

    public mutating func use(_ id: UInt64, now: Double) {
        with(id, now: now, create: false) { $0.usedAt = max($0.usedAt, now) }
    }

    public mutating func hover(_ id: UInt64, _ inside: Bool, now: Double) {
        with(id, now: now, create: false) {
            $0.hovered = inside
            $0.usedAt = max($0.usedAt, now)
        }
    }

    /// The shown checks that look at a camera (newest first).
    public func shown(on camera: AlertCamera) -> [VerifyCheck] { shown.filter { $0.cameras.contains(camera) } }
}

// MARK: The step log's words

/// Words for each trace step (the live view and the event log share them).
public enum TraceText {
    /// `time`: an event time (ms since 1970) as the clock text to show.
    public static func line(
        _ s: TraceStep, cameraName: (AlertCamera) -> String, time: (Int64) -> String = TraceText.clock
    ) -> String {
        let f = s.fields
        let cam = s.camera.map(cameraName)
        func secs(_ ms: Int64?) -> String {
            guard let ms else { return "?" }
            let v = Double(ms) / 1000
            return v == v.rounded() ? String(format: "%+.0f s", v) : String(format: "%+.1f s", v)
        }
        func cls(_ v: JSONValue?) -> String {
            let c = v?.string ?? "?"
            return c == "co" ? "CO" : c.replacingOccurrences(of: "_", with: " ")
        }
        switch s.step {
        case "trigger_received":
            let source =
                switch f["source"]?.string {
                case "device": "camera AI"
                case "local_audio": "sound"
                case "local_motion": "motion"
                case "external": "sensor"
                case "nvr_input": "NVR input"
                case let other: other ?? "?"
                }
            var t = "Trigger: \(source) \(cls(f["kind"]))"
            if let cam { t += " on \(cam)" }
            if let e = f["event_ms"]?.int64 { t += " at \(time(e))" }
            if f["late"]?.bool == true { t += ", \(Int((f["age_ms"]?.double ?? 0) / 1000)) s late" }
            return t
        case "retrigger":
            return "Retriggered: looking until \(secs(f["until_ms"]?.int64.map { $0 - s.atMs })) from now"
        case "gate":
            switch f["result"]?.string {
            case "pass": return "Gate: passed"
            case "pass_always": return "Gate: passed, an always-alert case (\(ruleWords(f["rule"]?.string)))"
            case "muted": return "Gate: the camera is muted" + (f["until_ms"]?.int64.map { " until \(time($0))" } ?? "")
            case "schedule": return "Gate: outside the camera's schedule"
            case "class_off": return "Gate: this class is off for the camera"
            case "cooldown": return "Gate: cooldown, \(Int((f["left_ms"]?.double ?? 0) / 1000)) s left"
            case "pass_cooldown":
                return "Gate: passed, checked and logged but not notified (cooldown, \(Int((f["left_ms"]?.double ?? 0) / 1000)) s left)"
            case "motion_only": return "Gate: motion only (the camera does not check motion)"
            case "duplicate": return "Gate: a duplicate trigger"
            case let other: return "Gate: \((other ?? "?").replacingOccurrences(of: "_", with: " "))"
            }
        case "sampling":
            let n = f["cameras"]?.array?.count ?? 0
            let classes = (f["classes"]?.array ?? []).map { cls($0) }.joined(separator: ", ")
            let window =
                "\(secs(f["from_ms"]?.int64.map { $0 - s.atMs })) to \(secs(f["until_ms"]?.int64.map { $0 - s.atMs }))"
            return "Looking at \(n) camera\(n == 1 ? "" : "s") for \(classes) from \(window); needs \(fmt(f["k"]?.double))"
        case "camera_woken":
            if let ms = f["first_picture_ms"]?.int64 { return "\(cam ?? "Camera") woken: first picture after \(ms) ms" }
            return "\(cam ?? "Camera") woken"
        case "frame_sampled":
            let dets = f["detections"]?.array ?? []
            var t = "\(cam ?? "Camera"): picture"
            if let fm = f["frame_ms"]?.int64 { t += " at \(time(fm))" }
            if dets.isEmpty {
                t += ": nothing found"
            } else {
                t += ": "
                    + dets.map { d -> String in
                        var w = "\(cls(d["class"])) \(String(format: "%.2f", d["score"]?.double ?? 0))"
                        if d["hit"]?.bool == true {
                            w += " hit"
                        } else if d["above"]?.bool == false {
                            w += " below floor"
                        } else if d["in_region"]?.bool == false {
                            w += " outside region"
                        }
                        return w
                    }.joined(separator: ", ")
            }
            if let more = f["more"]?.int64, more > 0 { t += " (+\(more) more)" }
            return t
        case "progress":
            return "\(cls(f["class"]).capitalized): \(fmt(f["weighted"]?.double)) of \(fmt(f["needed"]?.double)) "
                + "(\(f["hits"]?.int64 ?? 0) of \(f["n"]?.int64 ?? 0) pictures, \(f["angles"]?.int64 ?? 0) "
                + "angle\(f["angles"]?.int64 == 1 ? "" : "s"))"
        case "checked":
            let c = f["check"]
            var t: String
            switch c?["status"]?.string {
            case "verified":
                t = "Verified \(cls(c?["class"])) in \(c?["hits"]?.int64 ?? 0) of \(c?["n"]?.int64 ?? 0) pictures"
                    + " (\(String(format: "%.2f", c?["score"]?.double ?? 0)))"
            case "weak":
                t = "Weak: \(cls(c?["class"])) in \(c?["hits"]?.int64 ?? 0) of \(c?["n"]?.int64 ?? 0) pictures"
            case "not_seen":
                t = "Not seen in \(c?["n"]?.int64 ?? 0) pictures"
                if c?["outside_region"]?.bool == true { t += " (only outside the region)" }
            case "unavailable": t = "Could not look: no picture was checked"
            case let other: t = "Check: \(other ?? "?")"
            }
            switch f["agreed_with_device"]?.bool {
            case true?: t += "; the camera's AI agreed"
            case false?: t += "; the camera's AI disagreed"
            case nil: break
            }
            return t
        case "rules":
            var parts: [String] = []
            if f["night"]?.bool == true {
                let basis =
                    switch f["basis"]?.string {
                    case "sun": "by the sun"
                    case "fixed": "by the fixed hours"
                    case "mode": "night mode"
                    default: ""
                    }
                parts.append(basis.isEmpty ? "night" : "night (\(basis))")
            } else {
                parts.append("day")
            }
            if let e = f["exposure"]?.string { parts.append("\(e) camera") }
            if f["entrance"]?.bool == true { parts.append("entrance") }
            if let m = f["mode"]?.string { parts.append("mode \(m)") }
            if let sup = f["support"]?.string { parts.append(sup.replacingOccurrences(of: "_", with: " ")) }
            var t = "Rules: " + parts.joined(separator: ", ")
            if let sev = f["severity"]?.string { t += " → \(sev)" }
            if let a = f["always"]?.string { t += "; always alerts: \(ruleWords(a))" }
            if f["unverified"]?.bool == true { t += " (unverified)" }
            return t
        case "rarity":
            let one = f["one_in"]?.int64.map { "about 1 in \($0)" } ?? "no baseline"
            return "Rarity: \(one), \((f["effect"]?.string ?? "none").replacingOccurrences(of: "_", with: " "))"
        case "timeliness":
            let id = f["alert_id"]?.int64.map { "#\($0)" } ?? ""
            var t: String
            switch f["admission"]?.string {
            case "new": t = "New alert \(id)"
            case "update":
                t = "Update of alert \(id) (revision \(f["rev"]?.int64 ?? 0))"
                if let why = f["why"]?.string { t += ": \(why.replacingOccurrences(of: "_", with: " "))" }
            case "cooldown":
                t = "Alert \(id) already sent \(Int((f["since_ms"]?.double ?? 0) / 1000)) s ago; cooldown"
            case let other: t = "Timeliness: \(other ?? "?")"
            }
            if f["incident"]?.string != nil { t += ", same incident" }
            return t
        case "decision":
            switch f["decision"]?.string {
            case "alert":
                var t = "Decision: alert (\(f["severity"]?.string ?? "?"))"
                if f["late"]?.bool == true { t += ", late" }
                if f["unverified"]?.bool == true { t += ", unverified" }
                if f["quieted"]?["why"]?.string == "cooldown" { t += "; not notified: cooldown" }
                return t
            case "quiet": return "Decision: quiet log (\(reasonWords(f["reason"]?.string)))"
            case "suppress": return "Decision: suppressed (\(reasonWords(f["reason"]?.string)))"
            case let other: return "Decision: \(other ?? "?")"
            }
        case "delivery":
            let kind = outputWords(f["kind"]?.string)
            return f["ok"]?.bool == true ? "Delivered: \(kind)" : "Not delivered: \(kind)"
        case "omitted":
            return "\(f["steps"]?.int64 ?? 0) more steps not shown"
        default:
            return s.step.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    public static func ruleWords(_ rule: String?) -> String {
        switch rule {
        case "smoke_co": "smoke and CO alarms"
        case "night_outside_person": "a person on an outside camera at night"
        case "doorbell_night": "a doorbell press at night"
        case let other: (other ?? "?").replacingOccurrences(of: "_", with: " ")
        }
    }

    public static func reasonWords(_ reason: String?) -> String {
        switch reason {
        case "not_confirmed": "not confirmed"
        case "nothing_seen": "the cameras saw nothing"
        case "outside_region": "seen only outside the region"
        case "could_not_look": "could not look"
        case "low_severity": "low severity"
        case "muted": "camera muted"
        case "class_off": "class off"
        case let other: (other ?? "?").replacingOccurrences(of: "_", with: " ")
        }
    }

    public static func outputWords(_ kind: String?) -> String {
        switch kind {
        case "desktop": "desktop notification"
        case "home_assistant": "Home Assistant"
        case "ntfy": "ntfy"
        case "mqtt": "MQTT"
        case "webhook": "webhook"
        case let other: other ?? "?"
        }
    }

    private static func fmt(_ v: Double?) -> String {
        guard let v else { return "?" }
        return v == v.rounded() ? String(Int(v)) : String(format: "%.1f", v)
    }

    /// Local wall-clock time, `HH:mm:ss`.
    public static func clock(_ ms: Int64) -> String {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }
}
