import Foundation
import NVRCore

// Smart alerts (minor 12) in the fake core (docs/FFI.md "Smart alerts"),
// for the app's detector and views before they meet the real core: scripted
// verifications. A trigger of a detector class (person, vehicle, animal) or
// from an external source asks for two pictures (`verify_request` events
// with a small made-up H.264 keyframe); the app's answers decide it: a box
// of an allowed class at or above the floor (0.5) in both pictures alerts
// (`normal`), fewer is quiet (`not_confirmed`, or `nothing_seen` for a
// sensor). `expireAlertRequests()` stands for the answer timeout: a check
// with no answer at all is then `could_not_look` (quiet), or an unverified
// critical alert when `alwaysAlertCameras` holds its camera (an always-alert
// case failing open). Smoke and CO alert at once (critical); a mute
// suppresses everything on its camera. Events are queued by the calls and
// sent shortly after (or only on `completeAlertWork()` with
// `Options.recordingWorkIsManual`). Each check also sends its trace steps
// as `verify_step` events (trigger, gate, sampling, each picture, progress,
// the check, rules, timeliness, decision, and a desktop delivery for an
// alert) and logs them with the decision; `settleAlert` ends an alert's
// episode. `nv_alerts_get` filters like the core (camera, source, decision,
// class, severity, time, text). Not modeled: the engine's rules beyond
// these, episodes and cooldowns, outputs other than the desktop.

/// What the fake core keeps for smart alerts, under one lock.
final class FakeAlertsState: @unchecked Sendable {
    struct Check {
        var camera: AlertCamera?
        var className: String
        var external: Bool
        var source = "device"
        var answers: [Bool] = []
        var outstanding: Set<String> = []
        var steps: [[String: Any]] = []
        var always = false
    }

    struct World {
        var configured = false
        var enabled = false
        var alwaysAlertCameras: Set<AlertCamera> = []
        var nextTrigger: UInt64 = 1
        var nextRequest: UInt64 = 1
        var nextAlert: UInt64 = 1
        /// Alerts not settled yet: alert ID to trigger ID, camera and class.
        var openAlerts: [UInt64: (trigger: UInt64, camera: AlertCamera?, className: String)] = [:]
        var checks: [UInt64: Check] = [:]
        var requests: [String: UInt64] = [:]
        var log: [[String: Any]] = []
        var snapshots: [UInt64: Data] = [:]
        var feedback: [UInt64: [String: Any]] = [:]
        var mutes: [AlertCamera: Date] = [:]
        var events: [String] = []
    }

    private let lock = NSLock()
    private var world = World()

    func locked<R, E: Error>(_ body: (inout World) throws(E) -> R) throws(E) -> R {
        lock.lock()
        defer { lock.unlock() }
        return try body(&world)
    }
}

/// A made-up H.264 keyframe (start code, SPS, PPS, IDR header bytes): what
/// the fake hands out as a picture to check.
let fakeAlertKeyframe = Data([
    0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00,
])

extension FakeCore {
    private static func alertJSON(_ object: [String: Any]) -> String {
        let data =
            (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    private static func cameraObject(_ c: AlertCamera) -> [String: Any] {
        ["device": c.device, "channel": c.channel, "lens": c.lens]
    }

    private static func parseCamera(_ any: Any?) -> AlertCamera? {
        guard let o = any as? [String: Any], let d = o["device"] as? Int, let ch = o["channel"] as? Int,
            d >= 0, (0...Int(UInt16.max)).contains(ch)
        else { return nil }
        return AlertCamera(device: UInt64(d), channel: UInt16(ch), lens: UInt8((o["lens"] as? Int) ?? 0))
    }

    /// Cameras whose person checks are always-alert cases (they fail open
    /// when no picture could be checked).
    public func setAlwaysAlertCameras(_ cameras: Set<AlertCamera>) {
        alertsWorld.locked { $0.alwaysAlertCameras = cameras }
    }

    public func alertsConfigure(json: String) throws(CoreStatus) {
        checkOutsideCallback("nv_alerts_configure")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] else {
            throw .invalidArgument
        }
        if let e = o["enabled"], !(e is Bool) { throw .invalidArgument }
        let enabled = o["enabled"] as? Bool ?? true
        alertsWorld.locked { w in
            w.configured = true
            w.enabled = enabled
            if !enabled {
                w.checks.removeAll()
                w.requests.removeAll()
            }
            w.events.append(
                Self.alertJSON([
                    "type": "alerts_configured", "enabled": enabled, "issues": [String](),
                    "outputs": [Any](), "inputs": [Any](),
                ]))
        }
        scheduleAlertWork()
    }

    public func alertsTriggerPut(json: String) throws(CoreStatus) -> UInt64 {
        checkOutsideCallback("nv_alerts_trigger_put")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
            let source = o["source"] as? String, let kind = o["kind"] as? String, !kind.isEmpty
        else { throw .invalidArgument }
        let external = source == "external"
        let camera = Self.parseCamera(o["camera"])
        if external {
            guard o["origin"] is String else { throw .invalidArgument }
        } else if camera == nil {
            throw .invalidArgument
        }
        let className: String =
            switch kind.lowercased() {
            case "people", "person": "person"
            case "vehicle": "vehicle"
            case "dog_cat", "animal": "animal"
            case "smoke_alarm": "smoke"
            case "co_alarm": "co"
            default: external ? "sensor" : "other"
            }
        let id = try alertsWorld.locked { w throws(CoreStatus) -> UInt64 in
            guard w.configured, w.enabled else { throw .notConfigured }
            let id = w.nextTrigger
            w.nextTrigger += 1
            let now = Self.nowMs()
            var received: [String: Any] = [
                "at_ms": now, "step": "trigger_received", "source": source, "kind": kind, "class": className,
                "event_ms": now, "late": false, "age_ms": 0,
            ]
            if let camera { received["camera"] = Self.cameraObject(camera) }
            var check = FakeAlertsState.Check(camera: camera, className: className, external: external)
            check.source = source
            Self.step(&w, &check, id: id, received)
            if let c = camera, let until = w.mutes[c], until > Date() {
                let untilMs = Int(until.timeIntervalSince1970 * 1000)
                Self.step(&w, &check, id: id, ["at_ms": now, "step": "gate", "result": "muted", "until_ms": untilMs])
                Self.decide(&w, check, id: id, decision: "suppress", reason: "muted")
                return id
            }
            switch className {
            case "smoke", "co":
                check.always = true
                Self.step(&w, &check, id: id, ["at_ms": now, "step": "gate", "result": "pass_always", "rule": "smoke_co"])
                Self.decide(&w, check, id: id, decision: "alert", severity: "critical")
            case "person", "vehicle", "animal", "sensor":
                if let c = camera, className == "person", w.alwaysAlertCameras.contains(c) {
                    check.always = true
                    Self.step(
                        &w, &check, id: id,
                        ["at_ms": now, "step": "gate", "result": "pass_always", "rule": "night_outside_person"])
                } else {
                    Self.step(&w, &check, id: id, ["at_ms": now, "step": "gate", "result": "pass"])
                }
                Self.step(
                    &w, &check, id: id,
                    [
                        "at_ms": now, "step": "sampling", "cameras": camera.map { [Self.cameraObject($0)] } ?? [],
                        "from_ms": now - 2000, "until_ms": now + 8000,
                        "classes": external ? ["person", "vehicle", "animal"] : [className], "k": 2.0,
                    ])
                if let camera {
                    Self.step(
                        &w, &check, id: id,
                        ["at_ms": now, "step": "camera_woken", "camera": Self.cameraObject(camera), "first_picture_ms": 215])
                }
                w.checks[id] = check
                for _ in 0..<2 { Self.request(&w, check: id) }
            default:
                Self.step(&w, &check, id: id, ["at_ms": now, "step": "gate", "result": "unknown_kind"])
                Self.decide(&w, check, id: id, decision: "quiet", reason: "unknown_kind")
            }
            return id
        }
        scheduleAlertWork()
        return id
    }

    private static func nowMs() -> Int { Int(Date().timeIntervalSince1970 * 1000) }

    /// Records a trace step and sends it as a `verify_step` event.
    private static func step(
        _ w: inout FakeAlertsState.World, _ check: inout FakeAlertsState.Check, id: UInt64, _ step: [String: Any]
    ) {
        check.steps.append(step)
        w.events.append(alertJSON(["type": "verify_step", "trigger_id": id, "step": step]))
    }

    private static func request(_ w: inout FakeAlertsState.World, check: UInt64) {
        guard let c = w.checks[check] else { return }
        let key = "v\(w.nextRequest)"
        w.nextRequest += 1
        w.requests[key] = check
        w.checks[check]?.outstanding.insert(key)
        let camera = c.camera ?? AlertCamera(device: 0, channel: 0)
        w.events.append(
            alertJSON([
                "type": "verify_request", "request": key, "purpose": "check", "camera": cameraObject(camera),
                "trigger_ids": [check], "codec": "h264", "width": 1280, "height": 720,
                "pts_us": 1_000_000 * Int(w.nextRequest), "bytes": fakeAlertKeyframe.count,
                "frames": [["pts_us": 1_000_000 * Int(w.nextRequest), "bytes": fakeAlertKeyframe.count, "keyframe": true]],
                "from_cover": false, "at_ms": Int(Date().timeIntervalSince1970 * 1000),
                "deadline_ms": Int(Date().timeIntervalSince1970 * 1000) + 5000,
                "classes": c.external ? ["person", "vehicle", "animal"] : [c.className], "min_score": 0.5,
                "crops": [["what": "frame", "x": 0, "y": 0, "w": 1, "h": 1]],
            ]))
    }

    private static func decide(
        _ w: inout FakeAlertsState.World, _ check: FakeAlertsState.Check, id: UInt64, className: String? = nil,
        decision: String, severity: String? = nil, reason: String? = nil, unverified: Bool = false
    ) {
        var check = check
        let now = nowMs()
        let cls = className ?? check.className
        let name = check.camera.map { "Camera \($0.channel + 1)" } ?? "Sensor 1"
        let label = cls == "co" ? "CO" : cls.capitalized
        var d: [String: Any] = [
            "trigger_id": id, "class": cls, "decision": decision, "decided_ms": now,
            "explanation": decision == "alert"
                ? "\(label) on \(name) (fake core). Alerted." : "\(label) on \(name) (fake core). Not alerted: \(reason ?? "").",
        ]
        if let camera = check.camera { d["camera"] = cameraObject(camera) }
        var state = "logged"
        var outcome: [String: Any] = ["at_ms": now, "step": "decision", "decision": decision]
        if decision == "alert" {
            let alertID = w.nextAlert
            w.nextAlert += 1
            let sev = severity ?? "normal"
            d["alert_id"] = alertID
            d["rev"] = 1
            d["severity"] = sev
            d["late"] = false
            d["unverified"] = unverified
            if check.always { d["always"] = cls == "smoke" || cls == "co" ? "smoke_co" : "night_outside_person" }
            step(
                &w, &check, id: id,
                [
                    "at_ms": now, "step": "rules", "class": cls, "night": check.always, "basis": "fixed",
                    "exposure": "outside", "entrance": false, "mode": "home",
                    "support": unverified ? "unverified" : "verified", "severity": sev, "unverified": unverified,
                ])
            step(&w, &check, id: id, ["at_ms": now, "step": "timeliness", "admission": "new", "alert_id": alertID])
            outcome.merge(["severity": sev, "alert_id": alertID, "rev": 1, "late": false, "unverified": unverified]) { $1 }
            w.openAlerts[alertID] = (id, check.camera, cls)
            state = "new"
        } else {
            d["reason"] = reason ?? ""
            outcome["reason"] = reason ?? ""
        }
        step(&w, &check, id: id, outcome)
        d["trace"] = ["version": 1, "trigger_id": id, "steps": check.steps]
        var entry: [String: Any] = ["logged_ms": now, "source": check.source, "decision": d, "deliveries": [Any]()]
        w.events.append(alertJSON(["type": "alert", "state": state, "source": check.source, "decision": d]))
        if decision == "alert" {
            // The desktop output (the app's own): one delivery step.
            let delivery: [String: Any] = ["at_ms": now, "step": "delivery", "output": 0, "kind": "desktop", "ok": true]
            entry["deliveries"] = [delivery]
            w.events.append(alertJSON(["type": "verify_step", "trigger_id": id, "step": delivery]))
        }
        w.log.append(entry)
        if w.log.count > 1000 { w.log.removeFirst() }
    }

    /// Ends an alert's episode: the `settled` event the core sends when an
    /// episode settles.
    public func settleAlert(_ alertID: UInt64) {
        alertsWorld.locked { w in
            guard let open = w.openAlerts.removeValue(forKey: alertID) else { return }
            var e: [String: Any] = [
                "type": "alert", "state": "settled", "alert_id": alertID, "rev": 1, "trigger_id": open.trigger,
                "class": open.className, "at_ms": Self.nowMs(),
            ]
            if let c = open.camera { e["camera"] = Self.cameraObject(c) }
            w.events.append(Self.alertJSON(e))
        }
        scheduleAlertWork()
    }

    private static func finish(_ w: inout FakeAlertsState.World, check id: UInt64, timedOut: Bool) {
        guard var c = w.checks.removeValue(forKey: id) else { return }
        for k in c.outstanding { w.requests.removeValue(forKey: k) }
        let hits = c.answers.filter { $0 }.count
        let cls = c.external ? "person" : c.className
        let check: [String: Any] =
            c.answers.isEmpty
            ? ["status": "unavailable"]
            : hits >= 2
                ? ["status": "verified", "class": cls, "hits": hits, "n": c.answers.count, "angles": 1, "score": 0.8]
                : ["status": "not_seen", "n": c.answers.count, "outside_region": false]
        step(&w, &c, id: id, ["at_ms": nowMs(), "step": "checked", "check": check])
        if hits >= 2 {
            decide(&w, c, id: id, className: cls, decision: "alert", severity: c.always ? "critical" : "normal")
        } else if c.answers.isEmpty, timedOut, let cam = c.camera, w.alwaysAlertCameras.contains(cam) {
            decide(&w, c, id: id, decision: "alert", severity: "critical", unverified: true)
        } else if c.answers.isEmpty {
            decide(&w, c, id: id, decision: "quiet", reason: "could_not_look")
        } else {
            decide(&w, c, id: id, decision: "quiet", reason: c.external ? "nothing_seen" : "not_confirmed")
        }
    }

    public func alertFramesGet(request: String) throws(CoreStatus) -> AlertFrames {
        checkOutsideCallback("nv_alert_frames_get")
        if isFreed { throw .closed }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard w.requests[request] != nil else { throw .unknownID }
        }
        return AlertFrames(kind: .h264Keyframe, width: 1280, height: 720, ptsMicros: 0, data: fakeAlertKeyframe)
    }

    public func alertDetectionsPut(request: String, json: String) throws(CoreStatus) {
        checkOutsideCallback("nv_alert_detections_put")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
            let list = o["detections"] as? [[String: Any]]
        else { throw .invalidArgument }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard let id = w.requests.removeValue(forKey: request), var c = w.checks[id] else { throw .unknownID }
            let allowed: Set<String> = c.external ? ["person", "vehicle", "animal"] : [c.className]
            let hit = list.contains { d in
                let label = ((d["class"] ?? d["label"]) as? String)?.lowercased() ?? ""
                let cls =
                    switch label {
                    case "car", "truck", "bus", "motorcycle": "vehicle"
                    case "dog", "cat": "animal"
                    default: label
                    }
                return allowed.contains(cls) && ((d["score"] as? Double) ?? 0) >= 0.5
            }
            c.answers.append(hit)
            c.outstanding.remove(request)
            let now = Self.nowMs()
            let judged: [[String: Any]] = list.prefix(8).map { d in
                let label = ((d["class"] ?? d["label"]) as? String)?.lowercased() ?? ""
                let cls =
                    switch label {
                    case "car", "truck", "bus", "motorcycle": "vehicle"
                    case "dog", "cat": "animal"
                    default: label
                    }
                let score = (d["score"] as? Double) ?? 0
                return [
                    "class": cls, "score": score, "above": score >= 0.5, "in_region": true,
                    "hit": allowed.contains(cls) && score >= 0.5,
                ]
            }
            Self.step(
                &w, &c, id: id,
                [
                    "at_ms": now, "step": "frame_sampled",
                    "camera": Self.cameraObject(c.camera ?? AlertCamera(device: 0, channel: 0)), "frame_ms": now,
                    "detections": judged, "more": max(list.count - 8, 0),
                ])
            let hits = c.answers.filter { $0 }.count
            Self.step(
                &w, &c, id: id,
                [
                    "at_ms": now, "step": "progress", "class": c.external ? "person" : c.className,
                    "weighted": Double(hits), "needed": 2.0, "hits": hits, "n": c.answers.count, "angles": 1,
                ])
            w.checks[id] = c
            w.events.append(Self.alertJSON(["type": "verify_request_done", "request": request, "result": "answered"]))
            if c.outstanding.isEmpty { Self.finish(&w, check: id, timedOut: false) }
        }
        scheduleAlertWork()
    }

    /// The answer timeout: every request still unanswered expires and its
    /// check is decided with what it has.
    public func expireAlertRequests() {
        alertsWorld.locked { w in
            for (key, _) in w.requests {
                w.events.append(Self.alertJSON(["type": "verify_request_done", "request": key, "result": "timeout"]))
            }
            w.requests.removeAll()
            for id in Array(w.checks.keys) { Self.finish(&w, check: id, timedOut: true) }
        }
        scheduleAlertWork()
    }

    public func alertsGet(query: String?) throws(CoreStatus) -> String {
        checkOutsideCallback("nv_alerts_get")
        if isFreed { throw .closed }
        var q: [String: Any] = [:]
        if let query, !query.isEmpty {
            guard let o = (try? JSONSerialization.jsonObject(with: Data(query.utf8))) as? [String: Any] else {
                throw .invalidArgument
            }
            q = o
        }
        let decisions = q["decision"] as? [String]
        let sources = q["source"] as? [String]
        let classes = q["class"] as? [String]
        let camera = Self.parseCamera(q["camera"])
        let text = (q["text"] as? String)?.lowercased()
        let since = q["since_ms"] as? Int, until = q["until_ms"] as? Int, before = q["before_ms"] as? Int
        let ranks = ["low": 0, "normal": 1, "high": 2, "critical": 3]
        let minRank = (q["min_severity"] as? String).flatMap { ranks[$0] }
        let limit = min(q["limit"] as? Int ?? 100, 1000)
        return alertsWorld.locked { w in
            var entries: [[String: Any]] = []
            var more = false
            for e in w.log.reversed() {
                guard let d = e["decision"] as? [String: Any] else { continue }
                let decided = d["decided_ms"] as? Int ?? 0
                if let decisions, !decisions.contains(d["decision"] as? String ?? "") { continue }
                if let sources, !sources.contains(e["source"] as? String ?? "") { continue }
                if let classes, !classes.contains(d["class"] as? String ?? "") { continue }
                if let camera {
                    let c = Self.parseCamera(d["camera"])
                    if c?.device != camera.device || c?.channel != camera.channel { continue }
                }
                if let text, !((d["explanation"] as? String) ?? "").lowercased().contains(text) { continue }
                if let since, decided < since { continue }
                if let until, decided >= until { continue }
                if let before, decided >= before { continue }
                if let minRank, (ranks[d["severity"] as? String ?? ""] ?? -1) < minRank { continue }
                if entries.count >= limit {
                    more = true
                    break
                }
                var entry = e
                if let a = d["alert_id"] as? UInt64 {
                    entry["snapshot"] = w.snapshots[a] != nil
                    if let f = w.feedback[a] { entry["feedback"] = f }
                } else {
                    entry["snapshot"] = false
                }
                entries.append(entry)
            }
            let muted = w.mutes.filter { $0.value > Date() }.map {
                ["camera": Self.cameraObject($0.key), "until_ms": Int($0.value.timeIntervalSince1970 * 1000)] as [String: Any]
            }
            return Self.alertJSON([
                "entries": entries, "more": more,
                "status": [
                    "configured": w.configured, "enabled": w.enabled, "open_checks": w.checks.count,
                    "muted": muted, "requests": w.requests.count,
                ],
            ])
        }
    }

    public func alertSnapshotPut(alertID: UInt64, data: Data) throws(CoreStatus) {
        checkOutsideCallback("nv_alert_snapshot_put")
        if isFreed { throw .closed }
        guard !data.isEmpty else { throw .invalidArgument }
        guard data.count <= 1 << 20 else { throw .limit }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard alertID > 0, alertID < w.nextAlert else { throw .unknownID }
            w.snapshots[alertID] = data
        }
    }

    public func alertSnapshotGet(alertID: UInt64) throws(CoreStatus) -> Data {
        checkOutsideCallback("nv_alert_snapshot_get")
        if isFreed { throw .closed }
        return try alertsWorld.locked { w throws(CoreStatus) -> Data in
            guard let d = w.snapshots[alertID] else { throw .unknownID }
            return d
        }
    }

    public func alertFeedback(json: String) throws(CoreStatus) {
        checkOutsideCallback("nv_alert_feedback")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
            let id = o["alert_id"] as? Int, let vote = o["vote"] as? String, vote == "up" || vote == "down"
        else { throw .invalidArgument }
        if let r = o["reason"] {
            let known = ["nothing_there", "wrong_type", "not_interesting", "duplicate"]
            guard vote == "down", let r = r as? String, known.contains(r) else { throw .invalidArgument }
        }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard id > 0, UInt64(id) < w.nextAlert else { throw .unknownID }
            w.feedback[UInt64(id)] = o
        }
    }

    public func alertMute(cameraJSON: String, seconds: UInt32) throws(CoreStatus) {
        checkOutsideCallback("nv_alert_mute")
        if isFreed { throw .closed }
        guard let camera = Self.parseCamera(try? JSONSerialization.jsonObject(with: Data(cameraJSON.utf8))) else {
            throw .invalidArgument
        }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard w.configured else { throw .notConfigured }
            if seconds == 0 {
                w.mutes.removeValue(forKey: camera)
            } else {
                w.mutes[camera] = Date().addingTimeInterval(TimeInterval(min(seconds, 86_400)))
            }
        }
    }

    private func scheduleAlertWork() {
        guard !options.recordingWorkIsManual else { return }
        DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(10)) { [weak self] in
            self?.completeAlertWork()
        }
    }

    /// Sends the queued smart-alert events.
    public func completeAlertWork() {
        guard !isFreed else { return }
        let events = alertsWorld.locked { w -> [String] in
            defer { w.events.removeAll() }
            return w.events
        }
        for e in events { emit(event: e) }
    }
}
