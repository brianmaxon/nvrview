import CoreGraphics
import Foundation
import NVRCore
import Testing

// The smart-alert screens' pure parts (docs/UX.md "Smart alerts"): the
// settings and the core's document (no secret in the file), region
// geometry, the live view's lifetimes and stacking, the step log's words,
// and the event log's filters.

private let cam1 = CameraRef(device: "dev-a", channel: 1)
private let cam2 = CameraRef(device: "dev-a", channel: 2)
private let gone = CameraRef(device: "dev-gone", channel: 0)

private func object(_ json: String) -> [String: Any] {
    (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] ?? [:]
}

private func sampleSettings() -> SmartAlertSettings {
    var s = SmartAlertSettings()
    s.enabled = true
    s.location = AlertLocation(lat: 47.6543, lon: -122.3456)
    var c = AlertCameraSettings(camera: cam1)
    c.exposure = .perimeter
    c.entrance = true
    c.classes = [.vehicle, .person]
    c.zones = [AlertZone(points: [ROIPoint(x: 0, y: 0.5), ROIPoint(x: 1, y: 0.5), ROIPoint(x: 1, y: 1)], include: false)]
    c.schedule = .windows([AlertDailyWindow(days: 0b11111, startMin: 1320, endMin: 360)])
    s.setCamera(c)
    s.setCamera(AlertCameraSettings(camera: gone))
    var sensor = MQTTSensor(key: "sensor-1", name: "Sensor 1")
    sensor.host = "broker.local"
    sensor.topic = "example/sensors/motion"
    sensor.payload = .json
    sensor.jsonPath = "state"
    sensor.cameras = [
        SensorCamera(camera: cam1, roi: [ROIPoint(x: 0.1, y: 0.5), ROIPoint(x: 0.6, y: 0.5), ROIPoint(x: 0.6, y: 1)]),
        SensorCamera(camera: cam2), SensorCamera(camera: gone),
    ]
    s.sensors = [sensor]
    s.homeAssistant.enabled = true
    s.homeAssistant.baseURL = "http://homeassistant.local:8123"
    var rule = HARule(id: "r1", target: "input_boolean.example_alert")
    rule.classes = [.person]
    rule.cameras = [cam1, gone]
    rule.minSeverity = .high
    s.homeAssistant.rules = [rule]
    return s
}

private func context(secrets: Bool) -> SmartAlertSettings.CoreContext {
    SmartAlertSettings.CoreContext(
        deviceID: { $0 == "dev-a" ? 7 : nil }, name: { $0 == cam1 ? "Camera 2" : nil }, utcOffsetMinutes: -420,
        secret: secrets
            ? { s in
                switch s {
                case .mqttUser: "example-user"
                case .mqttPassword: "example-password"
                case .homeAssistantToken: "example-token"
                }
            } : nil)
}

@Suite struct SmartAlertSettingsTests {
    @Test func theFileRoundTripsAndHoldsNoSecret() throws {
        let s = sampleSettings()
        let data = s.fileData()
        #expect(SmartAlertSettings.fromFile(data) == s)
        let text = String(decoding: data, as: UTF8.self)
        for word in ["example-password", "example-token", "example-user", "password", "token"] {
            #expect(!text.contains(word), "the settings file holds \(word)")
        }
        // A file from an older or newer app: missing and unknown members.
        let partial = SmartAlertSettings.fromFile(Data(#"{"enabled":true,"mode":"away","future":{"x":1}}"#.utf8))
        #expect(partial.enabled && partial.mode == .away && partial.alwaysAlert == AlwaysAlertSettings())
        #expect(SmartAlertSettings.fromFile(Data("not json".utf8)) == SmartAlertSettings())
    }

    @Test func theOwnersDefaults() {
        let s = SmartAlertSettings()
        #expect(!s.enabled && s.mode == .home && s.location == nil)
        #expect(s.alwaysAlert.smokeCO && s.alwaysAlert.nightOutsidePerson && s.alwaysAlert.doorbellNight)
        #expect(s.night.afterSunsetMin == 30 && s.night.fallbackStartMin == 22 * 60 && s.night.fallbackEndMin == 6 * 60)
        #expect(s.desktop.enabled && s.desktop.pictures && s.homeAssistant.pictures && !s.homeAssistant.enabled)
        #expect(!s.standbyOnBattery && s.view.minSeconds == 60 && !s.view.showQuiet && s.view.placement == .tileThenInset)
        let sensor = MQTTSensor(key: "sensor-1", name: "")
        #expect(sensor.requireVerified && sensor.k == 2 && sensor.windowBeforeMs == 2000 && sensor.windowAfterMs == 8000)
        #expect(sensor.windowMaxMs == 30000 && sensor.debounceMs == 500 && sensor.retained == "ignore")
    }

    @Test func theLocationIsCoarse() {
        let l = AlertLocation(lat: 47.6543, lon: -122.3456)
        #expect(l.lat == 47.7 && l.lon == -122.3)
        #expect(AlertLocation(lat: 95, lon: 190).lat == 90 && AlertLocation(lat: 95, lon: 190).lon == -170)
        #expect(AlertLocation(lat: -0.04, lon: .nan) == AlertLocation(lat: 0, lon: 0))
        let decoded = try? JSONDecoder().decode(AlertLocation.self, from: Data(#"{"lat":10.06,"lon":20.04}"#.utf8))
        #expect(decoded == AlertLocation(lat: 10.1, lon: 20.0))
    }

    @Test func theCoreDocument() throws {
        let doc = object(sampleSettings().coreJSON(context(secrets: false)))
        #expect(doc["enabled"] as? Bool == true && doc["utc_offset_min"] as? Int == -420 && doc["mode"] as? String == "home")
        #expect((doc["location"] as? [String: Double]) == ["lat": 47.7, "lon": -122.3])
        let cameras = try #require(doc["cameras"] as? [[String: Any]])
        // The camera of a device not added now is left out.
        #expect(cameras.count == 1)
        let c = cameras[0]
        #expect((c["camera"] as? [String: Int]) == ["device": 7, "channel": 1, "lens": 0])
        #expect(c["name"] as? String == "Camera 2" && c["exposure"] as? String == "perimeter" && c["entrance"] as? Bool == true)
        #expect(c["classes"] as? [String] == ["person", "vehicle"])
        let zone = try #require((c["zones"] as? [[String: Any]])?.first)
        #expect(zone["include"] as? Bool == false && (zone["polygon"] as? [[Double]])?.count == 3)
        let schedule = try #require(c["schedule"] as? [String: Any])
        #expect(schedule["kind"] as? String == "windows")
        #expect((schedule["windows"] as? [[String: Int]]) == [["days": 31, "start_min": 1320, "end_min": 360]])
        let source = try #require((doc["sources"] as? [[String: Any]])?.first)
        #expect(source["key"] as? String == "sensor-1" && source["require"] as? String == "verified")
        #expect((source["cameras"] as? [[String: Any]])?.count == 2)
        #expect((source["cameras"] as? [[String: Any]])?.last?["roi"] as? [[Double]] == [])
        #expect(source["window_before_ms"] as? Int == 2000 && source["on_unavailable"] as? String == "quiet")
        let input = try #require((doc["inputs"] as? [[String: Any]])?.first)
        #expect(input["kind"] as? String == "mqtt" && input["credentials"] == nil)
        let mqtt = try #require(input["source"] as? [String: Any])
        #expect(mqtt["id"] as? String == "sensor-1" && mqtt["topic"] as? String == "example/sensors/motion")
        #expect((mqtt["broker"] as? [String: Any])?["host"] as? String == "broker.local")
        #expect((mqtt["mapping"] as? [String: Any])?["json_path"] as? String == "state")
        let outputs = try #require(doc["outputs"] as? [[String: Any]])
        #expect(outputs.map { $0["kind"] as? String } == ["desktop", "home_assistant"])
        #expect(outputs[1]["token"] == nil && outputs[1]["pictures"] as? Bool == true)
        let ha = try #require(outputs[1]["home_assistant"] as? [String: Any])
        let rule = try #require((ha["rules"] as? [[String: Any]])?.first)
        #expect((rule["action"] as? [String: String]) == ["kind": "pulse", "entity_id": "input_boolean.example_alert"])
        #expect((rule["cameras"] as? [[String: Int]])?.count == 1 && rule["min_severity"] as? String == "high")
    }

    @Test func secretsGoOnlyToTheCoreAndOnlyWhenAsked() throws {
        let s = sampleSettings()
        let withSecrets = object(s.coreJSON(context(secrets: true)))
        let input = try #require((withSecrets["inputs"] as? [[String: Any]])?.first)
        #expect((input["credentials"] as? [String: String]) == ["username": "example-user", "password": "example-password"])
        #expect((withSecrets["outputs"] as? [[String: Any]])?.last?["token"] as? String == "example-token")
        #expect(s.secrets == [.homeAssistantToken, .mqttUser("sensor-1"), .mqttPassword("sensor-1")])
        #expect(SmartAlertSecret.mqttPassword("sensor-1").account == "smart-alerts.mqtt.sensor-1.password")
    }

    @Test func offSendsOnlyEnabledFalse() {
        var s = sampleSettings()
        s.enabled = false
        let doc = object(s.coreJSON(context(secrets: true)))
        #expect(Set(doc.keys) == ["enabled", "utc_offset_min"] && doc["enabled"] as? Bool == false)
    }

    @Test func problemsInWords() {
        var s = sampleSettings()
        #expect(s.problems().isEmpty, "\(s.problems())")
        s.sensors[0].cameras = []
        s.sensors[0].topic = "example/+/motion"
        s.sensors[0].host = "mqtt.example.com"
        s.sensors.append(MQTTSensor(key: "sensor-1", name: "Second"))
        s.homeAssistant.baseURL = "http://ha.example.com:8123"
        s.homeAssistant.rules[0].target = "light.example"
        let p = s.problems()
        #expect(p.contains { $0.contains("at least one camera") })
        #expect(p.contains { $0.contains("wildcards") })
        #expect(p.contains { $0.contains("may not be on the local network") })
        #expect(p.contains { $0.contains("same key") })
        #expect(p.contains { $0.contains("http:// is only for hosts on the local network") })
        #expect(p.contains { $0.contains("input_boolean or switch") })
        #expect(MQTTSensor.validKey("sensor-1") && !MQTTSensor.validKey("Sensor 1") && !MQTTSensor.validKey(""))
        #expect(MQTTSensor.newKey(taken: ["sensor-2"]) == "sensor-3" && MQTTSensor.newKey(taken: []) == "sensor-1")
    }

    @Test func endpointsThatMayLeaveTheNetwork() {
        // Generic private, loopback, link-local and shared ranges (no real host).
        let privateRanges = [
            "http://10.0.0.2:8123", "https://172.16.4.1/x", "http://192.168.1.2",  // sanitize:allow
            "https://100.64.1.1", "http://169.254.1.1",  // sanitize:allow
        ]
        for local in privateRanges + [
            "http://127.0.0.1:1", "http://[fe80::1]:80/", "http://[fd00::1]", "http://homeassistant.local:8123",
            "http://server", "http://ha.home.arpa", "http://localhost:8123",
        ] {
            #expect(!AlertEndpoint.mayLeaveNetwork(local), "\(local)")
        }
        for remote in ["https://ha.example.com", "http://192.0.2.10", "http://203.0.113.5", "https://[2001:db8::1]", ""] {
            #expect(AlertEndpoint.mayLeaveNetwork(remote), "\(remote)")
        }
        #expect(AlertEndpoint.host(of: "https://name@HA.Example.com:8443/path?q") == "ha.example.com")
        #expect(AlertEndpoint.checkHomeAssistantURL("https://ha.example.com") == nil)
        #expect(AlertEndpoint.checkHomeAssistantURL("ftp://x")?.contains("http") == true)
        #expect(AlertEndpoint.checkHomeAssistantURL("https://u:p@ha.example.com")?.contains("user name") == true)
    }

    @Test func haRuleTargets() {
        var r = HARule(id: "r", target: "input_boolean.example_alert")
        #expect(HARuleCheck.problem(r) == nil)
        r.action = .script
        #expect(HARuleCheck.problem(r) != nil)
        r.target = "script.example"
        #expect(HARuleCheck.problem(r) == nil)
        r.action = .notify
        r.target = "mobile_app_example"
        #expect(HARuleCheck.problem(r) == nil)
        r.target = "Bad Name"
        #expect(HARuleCheck.problem(r) != nil)
    }
}

@Suite struct ROIGeometryTests {
    let fit = ROIGeometry.fitRect(picture: CGSize(width: 1280, height: 720), in: CGSize(width: 400, height: 400))

    @Test func fittingAndMapping() {
        #expect(fit == CGRect(x: 0, y: 87.5, width: 400, height: 225))
        let p = ROIGeometry.toPicture(CGPoint(x: 100, y: 87.5 + 112.5), fit: fit)
        #expect(p == ROIPoint(x: 0.25, y: 0.5))
        #expect(ROIGeometry.toView(p, fit: fit) == CGPoint(x: 100, y: 200))
        // Outside the picture clamps to its edge.
        #expect(ROIGeometry.toPicture(CGPoint(x: -20, y: 500), fit: fit) == ROIPoint(x: 0, y: 1))
        #expect(ROIGeometry.fitRect(picture: .zero, in: CGSize(width: 10, height: 10)).size == CGSize(width: 10, height: 10))
    }

    @Test func containsAndFeet() {
        let square = [ROIPoint(x: 0.1, y: 0.5), ROIPoint(x: 0.6, y: 0.5), ROIPoint(x: 0.6, y: 1), ROIPoint(x: 0.1, y: 1)]
        #expect(ROIGeometry.contains(square, ROIPoint(x: 0.3, y: 0.7)))
        #expect(!ROIGeometry.contains(square, ROIPoint(x: 0.7, y: 0.7)))
        #expect(!ROIGeometry.contains(Array(square.prefix(2)), ROIPoint(x: 0.3, y: 0.5)))
        // A tall box whose top is outside but whose feet are inside counts.
        let foot = ROIGeometry.foot(x: 0.2, y: 0.1, w: 0.1, h: 0.7)
        #expect(abs(foot.x - 0.25) < 1e-9 && abs(foot.y - 0.8) < 1e-9 && ROIGeometry.contains(square, foot))
        #expect(abs(ROIGeometry.area(square) - 0.25) < 1e-9)
    }

    @Test func theEditor() {
        var e = ROIEditor()
        e.press(at: CGPoint(x: 40, y: 100), fit: fit)
        e.release()
        e.press(at: CGPoint(x: 360, y: 100), fit: fit)
        e.release()
        #expect(!e.isClosed && e.saved.isEmpty)
        e.press(at: CGPoint(x: 360, y: 300), fit: fit)
        e.release()
        #expect(e.isClosed && e.saved.count == 3)
        // A press near a corner grabs it instead of adding one.
        e.press(at: CGPoint(x: 362, y: 302), fit: fit)
        e.drag(to: CGPoint(x: 500, y: 0), fit: fit)
        e.release()
        #expect(e.points.count == 3 && e.points[2] == ROIPoint(x: 1, y: 0))
        // Outside the picture adds nothing.
        e.press(at: CGPoint(x: 200, y: 20), fit: fit)
        #expect(e.points.count == 3 && e.grabbed == nil)
        e.deleteCorner()
        #expect(e.points.count == 2)
        for i in 0..<40 {
            e.press(at: CGPoint(x: 5 + Double(i % 25) * 15, y: 120 + Double(i / 25) * 40), fit: fit)
            e.release()
        }
        #expect(e.points.count == ROIGeometry.maxPoints)
        e.clear()
        #expect(e.points.isEmpty)
    }
}

private func step(_ json: String) -> JSONValue { JSONValue.parse(json)! }

private func request(_ key: String, trigger: UInt64, camera: Int = 2, classes: String = #"["person"]"#) -> VerifyRequest {
    let json = """
        {"request":"\(key)","purpose":"check","camera":{"device":1,"channel":\(camera),"lens":0},"trigger_ids":[\(trigger)],
         "codec":"h264","width":1280,"height":720,"pts_us":1,"bytes":1,"frames":[],"from_cover":false,"at_ms":1,
         "deadline_ms":2,"classes":\(classes),"min_score":0.5,"crops":[]}
        """
    return try! JSONDecoder().decode(VerifyRequest.self, from: Data(json.utf8))
}

private func decision(_ trigger: UInt64, _ kind: String, alertID: UInt64? = nil, extra: String = "") -> JSONValue {
    let id = alertID.map { #","alert_id":\#($0),"severity":"high","rev":1"# } ?? #","reason":"not_confirmed""#
    return step(
        #"{"type":"alert","state":"\#(kind == "alert" ? "new" : "logged")","decision":{"trigger_id":\#(trigger),"decision":"\#(kind)"\#(id),"explanation":"E."\#(extra)}}"#
    )
}

@Suite struct VerificationBoardTests {
    @Test func aCheckFromStepsBoxesAndDecision() {
        var b = VerificationBoard()
        b.step(triggerID: 12, step(#"{"at_ms":1000,"step":"trigger_received","source":"device","kind":"people","class":"person","camera":{"device":1,"channel":2,"lens":0},"event_ms":1000,"late":false,"age_ms":0}"#), now: 0)
        b.step(triggerID: 12, step(#"{"at_ms":1000,"step":"gate","result":"pass_always","rule":"night_outside_person"}"#), now: 0)
        let r = request("v1", trigger: 12)
        b.request(r, now: 0.2)
        let region = [ROIPoint(x: 0, y: 0.5), ROIPoint(x: 1, y: 0.5), ROIPoint(x: 1, y: 1), ROIPoint(x: 0, y: 1)]
        b.answered(
            r,
            detections: [
                AlertDetection(label: "person", score: 0.8, x: 0.4, y: 0.3, w: 0.1, h: 0.4),
                AlertDetection(label: "person", score: 0.3, x: 0.1, y: 0.3, w: 0.1, h: 0.4),
                AlertDetection(label: "person", score: 0.9, x: 0.1, y: 0.0, w: 0.1, h: 0.2),
                AlertDetection(label: "car", score: 0.9, x: 0.5, y: 0.5, w: 0.2, h: 0.2),
            ], region: { _ in region }, now: 0.3)
        let c = b.check(12)!
        #expect(c.cameras == [AlertCamera(device: 1, channel: 2)] && c.candidate && c.className == "person")
        let boxes = c.pictures[AlertCamera(device: 1, channel: 2)]!.boxes
        #expect(boxes.map(\.verdict) == [.hit, .belowFloor, .outsideRegion, .otherClass])
        #expect(boxes[0].caption == "person 0.80" && boxes[1].caption == "person 0.30 · below floor")
        #expect(boxes[3].alertClass == "vehicle")
        b.alertEvent(decision(12, "alert", alertID: 4), now: 1.3)
        #expect(b.check(12)?.isAlert == true && b.check(12)?.alertID == 4)
        #expect(b.check(12)?.headline(cameraName: { "Camera \($0.channel + 1)" }) == "Person · Camera 3 · alert (high)")
    }

    @Test func lifetimes() {
        var b = VerificationBoard()
        b.step(triggerID: 1, step(#"{"at_ms":0,"step":"gate","result":"pass"}"#), now: 0)
        // Pending: no deadline until the decision (or lost after 120 s).
        #expect(b.closesAt(b.check(1)!) == VerificationBoard.lostDecisionAfter + 60)
        b.alertEvent(decision(1, "alert", alertID: 9), now: 2)
        // An alert stays while its episode is active...
        b.tick(now: 100)
        #expect(b.check(1) != nil)
        b.alertEvent(step(#"{"type":"alert","state":"settled","alert_id":9,"trigger_id":1}"#), now: 200)
        // ...and at least 60 s after it ended.
        #expect(b.closesAt(b.check(1)!) == 260)
        b.tick(now: 259.9)
        #expect(b.check(1) != nil)
        // Using it starts the minute again; hovering holds it.
        b.use(1, now: 250)
        #expect(b.closesAt(b.check(1)!) == 310)
        b.hover(1, true, now: 300)
        b.tick(now: 10_000)
        #expect(b.check(1) != nil)
        b.hover(1, false, now: 10_000)
        #expect(b.tick(now: 10_060) == [1])
        // A longer minimum from the settings; never below 60 s.
        b.settings.minSeconds = 90
        b.step(triggerID: 2, step(#"{"at_ms":0,"step":"gate","result":"pass"}"#), now: 0)
        b.alertEvent(decision(2, "alert", alertID: 10), now: 0)
        b.alertEvent(step(#"{"type":"alert","state":"settled","alert_id":10}"#), now: 5)
        #expect(b.closesAt(b.check(2)!) == 95)
        b.settings.minSeconds = 10
        #expect(b.closesAt(b.check(2)!) == 65)
    }

    @Test func quietChecksShowOnlyWhenAsked() {
        var b = VerificationBoard()
        b.step(triggerID: 1, step(#"{"at_ms":0,"step":"gate","result":"pass"}"#), now: 0)
        #expect(b.shown.map(\.id) == [1])
        b.alertEvent(decision(1, "quiet"), now: 1)
        #expect(b.shown.isEmpty)
        #expect(b.tick(now: 1) == [1])
        b.settings.showQuiet = true
        b.step(triggerID: 2, step(#"{"at_ms":0,"step":"gate","result":"pass"}"#), now: 0)
        b.alertEvent(decision(2, "quiet"), now: 1)
        #expect(b.shown.map(\.id) == [2] && b.closesAt(b.check(2)!) == 61)
        // An always-alert candidate that ended quiet still shows.
        b.settings.showQuiet = false
        b.step(triggerID: 3, step(#"{"at_ms":0,"step":"gate","result":"pass_always","rule":"smoke_co"}"#), now: 0)
        b.alertEvent(decision(3, "quiet"), now: 1)
        #expect(b.shown.map(\.id) == [3])
    }

    @Test func stackingNewestFirstWithACount() {
        var b = VerificationBoard()
        for id in UInt64(1)...5 { b.step(triggerID: id, step(#"{"at_ms":0,"step":"gate","result":"pass"}"#), now: Double(id)) }
        #expect(b.visible.map(\.id) == [5, 4, 3] && b.more == 2)
        b.dismiss(5)
        #expect(b.visible.map(\.id) == [4, 3, 2] && b.more == 1)
        b.dismissAll()
        #expect(b.shown.isEmpty)
        for id in UInt64(1)...40 { b.step(triggerID: id, step(#"{"at_ms":0,"step":"gate","result":"pass"}"#), now: 0) }
        #expect(b.checks.count == VerificationBoard.maxChecks && b.checks.first?.id == 40)
    }

    @Test func aDecisionFillsInStepsMissedAndKeepsDeliveries() {
        var b = VerificationBoard()
        b.step(triggerID: 7, step(#"{"at_ms":5,"step":"delivery","output":0,"kind":"desktop","ok":true}"#), now: 0)
        let trace = #","trace":{"version":1,"trigger_id":7,"steps":[{"at_ms":1,"step":"trigger_received","source":"external","kind":"sensor_motion","class":"sensor","sensor":0,"event_ms":1,"late":false,"age_ms":0},{"at_ms":1,"step":"sampling","cameras":[{"device":1,"channel":3,"lens":0},{"device":1,"channel":4,"lens":0}],"from_ms":0,"until_ms":9,"classes":["person"],"k":2.0}]}"#
        b.alertEvent(decision(7, "alert", alertID: 3, extra: trace), now: 1)
        let c = b.check(7)!
        #expect(c.steps.map(\.step) == ["trigger_received", "sampling", "delivery"])
        #expect(c.sensor == 0 && c.cameras.count == 2 && c.source == "external")
    }

    @Test func placement() {
        #expect(VerifyPlace.place(.tileThenInset, onScreen: true) == .tile)
        #expect(VerifyPlace.place(.tileThenInset, onScreen: false) == .inset)
        #expect(VerifyPlace.place(.tileThenWindow, onScreen: false) == .window)
        #expect(VerifyPlace.place(.window, onScreen: true) == .window)
    }
}

@Suite struct TraceTextTests {
    let name: (AlertCamera) -> String = { "Camera \($0.channel + 1)" }
    let time: (Int64) -> String = { "t\($0)" }

    func line(_ json: String) -> String { TraceText.line(TraceStep(step(json))!, cameraName: name, time: time) }

    @Test func everyStepInWords() {
        #expect(
            line(#"{"at_ms":1,"step":"trigger_received","source":"device","kind":"people","class":"person","camera":{"device":1,"channel":3,"lens":0},"event_ms":5,"late":true,"age_ms":240000}"#)
                == "Trigger: camera AI people on Camera 4 at t5, 240 s late")
        #expect(line(#"{"at_ms":1,"step":"gate","result":"pass_always","rule":"night_outside_person"}"#)
            == "Gate: passed, an always-alert case (a person on an outside camera at night)")
        #expect(line(#"{"at_ms":1,"step":"gate","result":"cooldown","left_ms":28000}"#) == "Gate: cooldown, 28 s left")
        #expect(
            line(#"{"at_ms":1000,"step":"sampling","cameras":[{"device":1,"channel":3,"lens":0}],"from_ms":-1000,"until_ms":11000,"classes":["person"],"k":2.0}"#)
                == "Looking at 1 camera for person from -2 s to +10 s; needs 2")
        #expect(line(#"{"at_ms":1,"step":"camera_woken","camera":{"device":1,"channel":0,"lens":0},"first_picture_ms":215}"#)
            == "Camera 1 woken: first picture after 215 ms")
        #expect(
            line(#"{"at_ms":1,"step":"frame_sampled","camera":{"device":1,"channel":0,"lens":0},"frame_ms":9,"detections":[{"class":"person","score":0.75,"above":true,"in_region":true,"hit":true},{"class":"person","score":0.31,"above":false,"in_region":true,"hit":false},{"class":"vehicle","score":0.8,"above":true,"in_region":false,"hit":false}],"more":2}"#)
                == "Camera 1: picture at t9: person 0.75 hit, person 0.31 below floor, vehicle 0.80 outside region (+2 more)")
        #expect(line(#"{"at_ms":1,"step":"frame_sampled","camera":{"device":1,"channel":0,"lens":0},"detections":[]}"#)
            == "Camera 1: picture: nothing found")
        #expect(line(#"{"at_ms":1,"step":"progress","class":"person","weighted":1.0,"needed":2.0,"hits":1,"n":1,"angles":1}"#)
            == "Person: 1 of 2 (1 of 1 pictures, 1 angle)")
        #expect(
            line(#"{"at_ms":1,"step":"checked","check":{"status":"verified","class":"person","hits":2,"n":2,"angles":1,"score":0.75,"moving":null},"agreed_with_device":true}"#)
                == "Verified person in 2 of 2 pictures (0.75); the camera's AI agreed")
        #expect(line(#"{"at_ms":1,"step":"checked","check":{"status":"not_seen","n":3,"outside_region":true}}"#)
            == "Not seen in 3 pictures (only outside the region)")
        #expect(
            line(#"{"at_ms":1,"step":"rules","class":"person","night":true,"basis":"sun","exposure":"outside","entrance":false,"mode":"home","support":"verified","always":"night_outside_person","severity":"critical","unverified":false}"#)
                == "Rules: night (by the sun), outside camera, mode home, verified → critical; always alerts: a person on an outside camera at night")
        #expect(line(#"{"at_ms":1,"step":"timeliness","admission":"new","alert_id":4}"#) == "New alert #4")
        #expect(line(#"{"at_ms":1,"step":"timeliness","admission":"update","alert_id":4,"rev":2,"why":"still_present"}"#)
            == "Update of alert #4 (revision 2): still present")
        #expect(line(#"{"at_ms":1,"step":"decision","decision":"alert","severity":"critical","alert_id":4,"rev":1,"late":false,"unverified":true}"#)
            == "Decision: alert (critical), unverified")
        #expect(line(#"{"at_ms":1,"step":"decision","decision":"quiet","reason":"nothing_seen","hits":0,"n":3}"#)
            == "Decision: quiet log (the cameras saw nothing)")
        #expect(line(#"{"at_ms":1,"step":"delivery","output":1,"kind":"home_assistant","ok":false}"#)
            == "Not delivered: Home Assistant")
        #expect(line(#"{"at_ms":1,"step":"omitted","steps":12}"#) == "12 more steps not shown")
        #expect(line(#"{"at_ms":1,"step":"something_new"}"#) == "Something New")
    }
}

@Suite struct SmartAlertLogTests {
    static let page = #"""
        {"entries":[
         {"logged_ms":3000,"source":"external","decision":{"trigger_id":3,"class":"sensor","decision":"quiet","reason":"nothing_seen","explanation":"Sensor 1 fired at 13:00. Not alerted: the cameras did not confirm it.","decided_ms":3000,"trace":{"version":1,"trigger_id":3,"steps":[]}},"deliveries":[],"snapshot":false},
         {"logged_ms":2000,"source":"device","decision":{"trigger_id":2,"camera":{"device":1,"channel":2,"lens":0},"class":"person","decision":"alert","severity":"critical","alert_id":4,"rev":1,"late":false,"unverified":false,"always":"night_outside_person","explanation":"Person on Camera 3 at 22:30 (night). Verified in 2 of 2 pictures.","decided_ms":2000,"trace":{"version":1,"trigger_id":2,"steps":[{"at_ms":1,"step":"gate","result":"pass"}]}},"deliveries":[{"at_ms":5,"step":"delivery","output":1,"kind":"home_assistant","ok":true}],"snapshot":true,"feedback":{"alert_id":4,"vote":"down","reason":"nothing_there"}},
         {"logged_ms":1000,"source":"local_audio","decision":{"trigger_id":1,"camera":{"device":1,"channel":0,"lens":0},"class":"smoke","decision":"suppress","reason":"muted","explanation":"Smoke alarm on Camera 1. Muted.","decided_ms":1000}}
        ],"more":true,"status":{"configured":true,"muted":[{"camera":{"device":1,"channel":0,"lens":0},"until_ms":99}]}}
        """#

    @Test func entries() throws {
        let p = try #require(SmartAlertLogPage.parse(Self.page))
        #expect(p.entries.count == 3 && p.more)
        let a = p.entries[1]
        #expect(a.alertID == 4 && a.camera == AlertCamera(device: 1, channel: 2) && a.hasSnapshot && a.always != nil)
        #expect(a.vote == "down" && a.voteReason == "nothing_there" && a.sourceGroup == .cameraAI)
        #expect(a.firstSentence == "Person on Camera 3 at 22:30 (night).")
        #expect(a.allSteps.map(\.step) == ["gate", "delivery"])
        #expect(p.entries[0].sourceGroup == .sensor && p.entries[2].sourceGroup == .sound)
        #expect(p.muted.count == 1 && p.muted[0].untilMs == 99)
    }

    @Test func filtersAndTheirQuery() throws {
        let entries = try #require(SmartAlertLogPage.parse(Self.page)).entries
        var f = SmartAlertLogFilter()
        #expect(f.query() == #"{"limit":200}"#)
        #expect(entries.filter(f.matches).count == 3)
        f.decisions = ["alert"]
        #expect(entries.filter(f.matches).map(\.triggerID) == [2])
        f.decisions = []
        f.sources = [.sensor, .sound]
        #expect(entries.filter(f.matches).map(\.triggerID) == [3, 1])
        f.sources = []
        f.camera = AlertCamera(device: 1, channel: 2)
        f.classes = ["person"]
        f.minSeverity = .high
        f.text = "night"
        f.sinceMs = 1500
        f.untilMs = 2500
        #expect(entries.filter(f.matches).map(\.triggerID) == [2])
        f.text = "nothing like it"
        #expect(entries.filter(f.matches).isEmpty)
        let q = object(f.query(beforeMs: 2000))
        #expect((q["camera"] as? [String: Int]) == ["device": 1, "channel": 2, "lens": 0])
        #expect(q["class"] as? [String] == ["person"] && q["min_severity"] as? String == "high")
        #expect(q["since_ms"] as? Int == 1500 && q["until_ms"] as? Int == 2500 && q["before_ms"] as? Int == 2000)
        var g = SmartAlertLogFilter()
        g.sources = [.cameraAI]
        g.decisions = ["quiet", "suppress"]
        let q2 = object(g.query())
        #expect(q2["source"] as? [String] == ["device", "local_motion", "nvr_input"])
        #expect(q2["decision"] as? [String] == ["quiet", "suppress"])
    }

    @Test func feedbackAndMutes() {
        #expect(AlertFeedbackReason.json(alertID: 4, useful: true, reason: .duplicate) == #"{"alert_id":4,"vote":"up"}"#)
        #expect(
            AlertFeedbackReason.json(alertID: 4, useful: false, reason: .nothingThere)
                == #"{"alert_id":4,"reason":"nothing_there","vote":"down"}"#)
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(secondsFromGMT: 0)!
        let evening = Date(timeIntervalSince1970: 22 * 3600)  // 22:00
        #expect(AlertMuteChoice.untilMorning.seconds(now: evening, calendar: cal) == 9 * 3600)
        #expect(AlertMuteChoice.minutes15.seconds(now: evening) == 900 && AlertMuteChoice.hour1.seconds(now: evening) == 3600)
    }
}
