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
// `Options.recordingWorkIsManual`). Not modeled: the engine's rules beyond
// these, episodes and cooldowns, settling, outputs.

/// What the fake core keeps for smart alerts, under one lock.
final class FakeAlertsState: @unchecked Sendable {
    struct Check {
        var camera: AlertCamera?
        var className: String
        var external: Bool
        var answers: [Bool] = []
        var outstanding: Set<String> = []
    }

    struct World {
        var configured = false
        var enabled = false
        var alwaysAlertCameras: Set<AlertCamera> = []
        var nextTrigger: UInt64 = 1
        var nextRequest: UInt64 = 1
        var nextAlert: UInt64 = 1
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
            if let c = camera, let until = w.mutes[c], until > Date() {
                Self.decide(&w, id: id, camera: c, className: className, decision: "suppress", reason: "muted")
                return id
            }
            switch className {
            case "smoke", "co":
                Self.decide(&w, id: id, camera: camera, className: className, decision: "alert", severity: "critical")
            case "person", "vehicle", "animal", "sensor":
                w.checks[id] = FakeAlertsState.Check(camera: camera, className: className, external: external)
                for _ in 0..<2 { Self.request(&w, check: id) }
            default:
                Self.decide(&w, id: id, camera: camera, className: className, decision: "quiet", reason: "unknown_kind")
            }
            return id
        }
        scheduleAlertWork()
        return id
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
        _ w: inout FakeAlertsState.World, id: UInt64, camera: AlertCamera?, className: String, decision: String,
        severity: String? = nil, reason: String? = nil, unverified: Bool = false
    ) {
        var d: [String: Any] = [
            "trigger_id": id, "class": className, "decision": decision,
            "decided_ms": Int(Date().timeIntervalSince1970 * 1000),
            "explanation": decision == "alert" ? "Alerted (fake core)." : "Not alerted (fake core).",
            "trace": ["version": 1, "trigger_id": id, "steps": [[String: Any]]()],
        ]
        if let camera { d["camera"] = cameraObject(camera) }
        var state = "logged"
        if decision == "alert" {
            d["alert_id"] = w.nextAlert
            d["rev"] = 1
            d["severity"] = severity ?? "normal"
            d["late"] = false
            d["unverified"] = unverified
            w.nextAlert += 1
            state = "new"
        } else {
            d["reason"] = reason ?? ""
        }
        w.log.append(d)
        if w.log.count > 1000 { w.log.removeFirst() }
        w.events.append(alertJSON(["type": "alert", "state": state, "source": "device", "decision": d]))
    }

    private static func finish(_ w: inout FakeAlertsState.World, check id: UInt64, timedOut: Bool) {
        guard let c = w.checks.removeValue(forKey: id) else { return }
        for k in c.outstanding { w.requests.removeValue(forKey: k) }
        let hits = c.answers.filter { $0 }.count
        if hits >= 2 {
            Self.decide(
                &w, id: id, camera: c.camera, className: c.external ? "person" : c.className, decision: "alert")
        } else if c.answers.isEmpty, timedOut, let cam = c.camera, w.alwaysAlertCameras.contains(cam) {
            Self.decide(
                &w, id: id, camera: c.camera, className: c.className, decision: "alert", severity: "critical",
                unverified: true)
        } else if c.answers.isEmpty {
            Self.decide(&w, id: id, camera: c.camera, className: c.className, decision: "quiet", reason: "could_not_look")
        } else {
            Self.decide(
                &w, id: id, camera: c.camera, className: c.external ? "sensor" : c.className, decision: "quiet",
                reason: c.external ? "nothing_seen" : "not_confirmed")
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
        let limit = min(q["limit"] as? Int ?? 100, 1000)
        return alertsWorld.locked { w in
            var entries: [[String: Any]] = []
            for d in w.log.reversed() {
                if let decisions, !decisions.contains(d["decision"] as? String ?? "") { continue }
                if entries.count >= limit { break }
                var e: [String: Any] = ["decision": d, "source": "device", "deliveries": [Any]()]
                if let a = d["alert_id"] as? UInt64 {
                    e["snapshot"] = w.snapshots[a] != nil
                    if let f = w.feedback[a] { e["feedback"] = f }
                } else {
                    e["snapshot"] = false
                }
                entries.append(e)
            }
            let muted = w.mutes.filter { $0.value > Date() }.map { Self.cameraObject($0.key) }
            return Self.alertJSON([
                "entries": entries, "more": false,
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
