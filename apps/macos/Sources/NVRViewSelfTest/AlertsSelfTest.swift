@testable import NVRViewApp
import AlertHost
import DetectorKit
import Foundation
import NVRCore
import Synchronization
import AppBase
import AppAlerts

/// Self-test steps `alerts-e2e-verified`, `alerts-e2e-not-confirmed` and
/// `alerts-ui-real-core`, and the debugging and review steps
/// `alerts-sensor-messages`, `alerts-sensor-diagnostics`,
/// `alerts-simulate-trigger`, `alerts-ha-test-alert`, `alerts-evidence-kept`
/// and `alerts-events-overlay` (docs/NOTIFICATIONS.md): smart alerts end to end on the real core against
/// the mock NVR, with the app's own host in the middle. A trigger (the path
/// an MQTT message takes after the input module) wakes a mapped camera; the
/// core announces pictures; the host decodes them with VideoToolbox and the
/// detector answers; the core decides; a Home Assistant output (the core's
/// own module, against a mock of the REST API on this computer) is pulsed
/// for an alert and not otherwise; the host's notification carries the
/// explanation and a snapshot.
///
/// The verified step uses a stub detector that reports a person (the mock's
/// pictures are synthetic); the not-confirmed step uses the real model when
/// it is present, which finds nothing in a synthetic picture, else a stub
/// that finds nothing. The third step goes through the app's own path:
/// settings saved in Settings > Smart Alerts (the token through its
/// credential store), read back by the host from their stored form, and the
/// verified alert reaching the Events window. Runs only on the real core,
/// against a device on this computer.
@MainActor
final class AlertsSelfTest {
    private let model: AppModel
    private var problems: [String] = []
    private static let helper = "input_boolean.example_alert"
    private static let token = "example-token-0123456789"

    /// Opens the Smart Alert Events window (the app delegate's).
    private let openEvents: () -> SmartAlertEventsWindowController?

    init(model: AppModel, openEvents: @escaping () -> SmartAlertEventsWindowController? = { nil }) {
        self.model = model
        self.openEvents = openEvents
    }

    /// Reports why the steps cannot run here, else nil.
    static func unavailable(_ model: AppModel) -> String? {
        if model.fake != nil { return "the fake core has no outputs" }
        guard let device = model.devices.first(where: { $0.link.state == .connected && $0.deviceID != nil }),
            ["127.0.0.1", "localhost", "::1"].contains(device.config.host)
        else { return "no connected device on this computer" }
        return device.channels.count >= 2 ? nil : "the device has fewer than two channels"
    }

    func run() async -> [String] {
        guard let device = model.devices.first(where: { $0.link.state == .connected && $0.deviceID != nil }),
            let ha = MockHomeAssistant(token: Self.token, entities: [Self.helper: "off"])
        else { return ["alerts: could not start"] }
        defer {
            ha.stop()
            model.smartAlerts = nil
            // Nothing stays woken after the steps.
            try? model.bridge.smartAlerts().alertsConfigure(json: #"{"enabled":false}"#)
        }
        let channels = device.channels.map(\.channel).sorted()
        // Step alerts-e2e-verified (the parity markers are in SelfTest.swift).
        await scenario(
            "alerts-e2e-verified", device: device, channel: channels[0], ha: ha, personVisible: true,
            expectAlert: true)
        // Step alerts-e2e-not-confirmed.
        await scenario(
            "alerts-e2e-not-confirmed", device: device, channel: channels[1], ha: ha, personVisible: false,
            expectAlert: false)
        // Step alerts-ui-real-core, on a camera without an open episode (an
        // alert on the first one would be an update of the first step's).
        await uiScenario(device: device, channel: channels.count > 2 ? channels[2] : channels[1], ha: ha)
        // Steps alerts-cooldown-logs-second, alerts-night-evidence-expiry and
        // alerts-cooldown-always-alert.
        await cooldownScenario(device: device, channel: channels[0])
        return problems
    }

    private func fail(_ id: String, _ text: String) { problems.append("\(id): \(text)") }

    private func scenario(
        _ id: String, device: DeviceRuntime, channel: Int, ha: MockHomeAssistant, personVisible: Bool,
        expectAlert: Bool
    ) async {
        let before = ha.turnOns(Self.helper)
        let camera = #"{"device_key":"\#(device.key)","channel":\#(channel)}"#
        let settings = """
            {"enabled":true,"cameras":[{"camera":\(camera)}],
             "always_alert":{"night_outside_person":false,"doorbell_night":false},
             "sources":[{"key":"sensor-1","name":"Sensor 1","cameras":[{"camera":\(camera)}]}],
             "outputs":[{"kind":"desktop"},{"kind":"home_assistant","id":"ha","endpoint":"\(ha.url)",
               "min_severity":"low","pulse_gap_ms":0,"cooldown_ms":0,
               "rules":[{"action":{"kind":"pulse","entity_id":"\(Self.helper)"}}]}]}
            """
        let notifier = RecordingNotifier()
        let bridge = model.bridge
        let modelDevice = model
        let environment = SmartAlertsController.Environment(
            core: { try bridge.smartAlerts() },
            storedSettings: { Data(settings.utf8) },
            deviceID: { key in modelDevice.runtime(key)?.deviceID },
            secrets: MemorySecrets(["ha": Self.token]))
        // The real model answers the not-confirmed step when it is present.
        let real = personVisible ? nil : DetectorModelLocator.locate()
        let installer = ModelInstaller(manifest: nil, locate: { real })
        let detector: (@Sendable () async -> DetectorHandle?)?
        if real != nil {
            detector = nil  // the controller loads the real model
        } else {
            let stub = StubDetector(person: personVisible)
            let handle = DetectorHandle(service: DetectorService(detector: stub), labels: DetectorProvider.cocoLabels)
            detector = { handle }
        }
        let controller = SmartAlertsController(
            environment: environment, installer: installer, notifier: notifier, detector: detector)
        model.smartAlerts = controller
        controller.configure()
        guard await wait(30, { if case .running = controller.state { true } else { false } }) else {
            return fail(id, "the core was not configured (\(controller.state))")
        }
        guard let core = try? bridge.smartAlerts(),
            let trigger = try? core.alertsTriggerPut(
                json: #"{"source":"external","origin":"sensor-1","kind":"sensor_motion","state":"pulse"}"#)
        else { return fail(id, "the trigger was refused") }
        let query = #"{"trigger_id":\#(trigger)}"#
        var entry: [String: Any]?
        let decided = await wait(60) {
            guard let text = try? core.alertsGet(query: query),
                let o = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any],
                let first = (o["entries"] as? [[String: Any]])?.first
            else { return false }
            entry = first
            return true
        }
        guard decided, let entry, let decision = entry["decision"] as? [String: Any] else {
            return fail(id, "no decision was made for the trigger (host state \(controller.state), model \(controller.modelStatus.message))")
        }
        let kind = decision["decision"] as? String ?? "?"
        print("\(id): \(kind) \(decision["reason"] as? String ?? decision["severity"] as? String ?? ""), \(real != nil ? "real" : "stub") detector")
        if expectAlert {
            guard kind == "alert" else { return fail(id, "expected an alert, got \(kind) \(decision["reason"] ?? "")") }
            if decision["unverified"] as? Bool != false { fail(id, "the alert is marked unverified") }
            // The pulse reached the Home Assistant mock, with the token.
            guard await wait(30, { ha.turnOns(Self.helper) == before + 1 && ha.entityState(Self.helper) == "off" }) else {
                return fail(id, "Home Assistant was not pulsed: \(ha.log)")
            }
            if ha.log.contains(where: { $0.contains("bad token") }) { fail(id, "Home Assistant rejected the token") }
            // The delivery is a trace step, and the log has it.
            var delivered: [String: Any]?
            _ = await wait(15) {
                guard let text = try? core.alertsGet(query: query),
                    let o = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any],
                    let steps = (o["entries"] as? [[String: Any]])?.first?["deliveries"] as? [[String: Any]]
                else { return false }
                delivered = steps.first
                return delivered != nil
            }
            if delivered?["ok"] as? Bool != true || delivered?["kind"] as? String != "home_assistant" {
                fail(id, "the delivery step is \(String(describing: delivered))")
            }
            // The desktop notification: the explanation, a snapshot.
            guard await wait(15, { !notifier.notifications.isEmpty }) else { return fail(id, "no notification was raised") }
            let note = notifier.notifications[0]
            if note.body.isEmpty || note.body != decision["explanation"] as? String { fail(id, "the notification lacks the explanation") }
            // The title leads with the camera's name ("Camera N" when the settings give none).
            if !note.title.hasPrefix("Camera \(channel + 1): ") {
                fail(id, "the notification title does not lead with the camera: \(note.title)")
            }
            if note.snapshot?.starts(with: [0xFF, 0xD8]) != true { fail(id, "the notification has no snapshot") }
            if let alertID = decision["alert_id"] as? Int,
                (try? core.alertSnapshotGet(alertID: UInt64(alertID)))?.starts(with: [0xFF, 0xD8]) != true
            {
                fail(id, "the core did not keep the snapshot")
            }
            print("\(id): pulsed Home Assistant, notified \"\(note.title)\", snapshot \(note.snapshot?.count ?? 0) bytes")
            await evidenceKept(trigger: trigger, decision: "alert", boxes: true)
        } else {
            guard kind == "quiet" else { return fail(id, "expected a quiet decision, got \(kind)") }
            // Proving that nothing happens takes a short fixed wait: a pulse
            // would start within milliseconds of the decision.
            try? await Task.sleep(for: .milliseconds(400))
            if ha.turnOns(Self.helper) != before { fail(id, "Home Assistant was pulsed for an unconfirmed trigger") }
            if !notifier.notifications.isEmpty { fail(id, "a notification was raised for an unconfirmed trigger") }
            print("\(id): quiet, no pulse, no notification")
            await evidenceKept(trigger: trigger, decision: "quiet", boxes: false)
            await motionOnlyKeepsNothing(device: device, channel: channel)
        }
        let configured = controller.lastConfigured ?? ""
        if configured.contains(Self.token) { fail(id, "the token appeared in an event") }
        if let text = try? core.alertsGet(query: "{}"), text.contains(Self.token) { fail(id, "the token appeared in the alert log") }
    }

    /// Settings > Smart Alerts to the Events window, with the app's host in
    /// between (the stub detector reports a person).
    private func uiScenario(device: DeviceRuntime, channel: Int, ha: MockHomeAssistant) async {
        let id = "alerts-ui-real-core"
        let alerts = model.alertsModel!
        let before = ha.turnOns(Self.helper)
        let notifier = RecordingNotifier()
        let bridge = model.bridge
        let stub = StubDetector(person: true)
        let handle = DetectorHandle(service: DetectorService(detector: stub), labels: DetectorProvider.cocoLabels)
        let environment = SmartAlertsController.Environment(
            core: { try bridge.smartAlerts() },
            storedSettings: { [weak model] in model?.alertsModel.storedData },
            deviceID: { [weak model] key in model?.runtime(key)?.deviceID },
            secrets: ModelAlertSecrets(model: model))
        let controller = SmartAlertsController(
            environment: environment, installer: ModelInstaller(manifest: nil, locate: { nil }), notifier: notifier,
            detector: { handle })
        model.smartAlerts = controller
        var steps = 0
        let token = alerts.subscribeEvents { e in if case .verifyStep = e { steps += 1 } }
        let camera = CameraRef(device: device.key, channel: channel)
        defer {
            alerts.unsubscribeEvents(token)
            alerts.setSecret("", for: .homeAssistantToken)
            alerts.update { $0 = SmartAlertSettings() }
        }
        // What the user does in Settings: a camera, a sensor that it confirms
        // (its broker is a closed local port: the trigger is put directly),
        // Home Assistant with a rule and its token.
        alerts.setSecret(Self.token, for: .homeAssistantToken)
        alerts.update { s in
            s.enabled = true
            s.setCamera(AlertCameraSettings(camera: camera))
            var sensor = MQTTSensor(key: "sensor-ui", name: "Sensor UI")
            sensor.host = "127.0.0.1"
            sensor.port = 9
            sensor.topic = "example/sensors/ui"
            sensor.cameras = [SensorCamera(camera: camera)]
            s.sensors = [sensor]
            s.homeAssistant.enabled = true
            s.homeAssistant.baseURL = ha.url
            s.homeAssistant.minSeverity = .low
            s.homeAssistant.pulseGapMs = 0
            s.homeAssistant.cooldownMs = 0
            s.homeAssistant.rules = [HARule(id: "r1", target: Self.helper)]
        }
        // The host read the stored form: cameras by device key, no secret.
        let stored = alerts.storedData.map { String(decoding: $0, as: UTF8.self) } ?? ""
        if !stored.contains("\"device_key\"") || stored.contains(Self.token) {
            fail(id, "the stored settings are \(stored.isEmpty ? "missing" : "malformed or hold the token")")
        }
        guard await wait(30, { if case .running = controller.state { true } else { false } }) else {
            return fail(id, "the host did not configure the core from the saved settings (\(controller.state))")
        }
        guard let core = try? bridge.smartAlerts(),
            let trigger = try? core.alertsTriggerPut(
                json: #"{"source":"external","origin":"sensor-ui","kind":"sensor_motion","state":"pulse"}"#)
        else { return fail(id, "the trigger was refused") }
        guard let events = openEvents()?.events else { return fail(id, "the Events window did not open") }
        var shown: SmartAlertLogEntry?
        let listed = await wait(60) {
            shown = events.entries.first { $0.triggerID == trigger && $0.decision == "alert" }
            return shown != nil
        }
        guard listed, let shown else {
            let got = events.entries.first { $0.triggerID == trigger }
            return fail(id, "the Events window did not list an alert for the trigger (\(got?.decision ?? "no entry"))")
        }
        if steps == 0 { fail(id, "the live checks got no verification step") }
        guard await wait(30, { ha.turnOns(Self.helper) == before + 1 }) else {
            let configured = controller.lastConfigured ?? "none"
            let deliveries = (try? core.alertsGet(query: #"{"trigger_id":\#(trigger)}"#)).map { String($0.prefix(600)) } ?? "-"
            return fail(
                id,
                "Home Assistant was not pulsed with the token saved in Settings: \(ha.log); token saved \(alerts.hasSecret(.homeAssistantToken)); configured \(configured); log \(deliveries)")
        }
        if (try? core.alertsGet(query: "{}"))?.contains(Self.token) == true { fail(id, "the token appeared in the alert log") }
        print("\(id): saved settings configured the core, Events lists \"\(shown.explanation)\", \(steps) live steps, pulsed")
        await reviewSteps(
            alerts: alerts, core: core, ha: ha, events: events, stub: stub, controller: controller, shown: shown,
            sensor: "sensor-ui", camera: camera,
            other: device.channels.map(\.channel).sorted().dropFirst().first.map { CameraRef(device: device.key, channel: $0) })
    }

    /// A detection inside a cooldown is logged with its evidence but does not
    /// notify; night evidence outlives the ordinary retention; an always-alert
    /// case inside a cooldown still alerts. Device-source triggers (the camera
    /// AI's path) on one outside camera, the cooldown's spacing between
    /// checks set to zero so that no step waits on the real clock (the
    /// spacing and the 10 s case are tested on the simulated clock in the
    /// core's tests); the night is the whole day (equal fixed hours).
    private func cooldownScenario(device: DeviceRuntime, channel: Int) async {
        let logsSecond = "alerts-cooldown-logs-second"
        let expiry = "alerts-night-evidence-expiry"
        let always = "alerts-cooldown-always-alert"
        guard let deviceID = device.deviceID else { return fail(logsSecond, "the device has no core ID") }
        let camera = #"{"device_key":"\#(device.key)","channel":\#(channel)}"#
        let settings = """
            {"enabled":true,"cameras":[{"camera":\(camera),"exposure":"outside"}],
             "night":{"fallback_start_min":360,"fallback_end_min":360},
             "timing":{"check_spacing_s":0},
             "evidence":{"retention_min":30,"night_hold_h":6},
             "outputs":[{"kind":"desktop"}]}
            """
        let notifier = RecordingNotifier()
        let bridge = model.bridge
        let modelDevice = model
        let environment = SmartAlertsController.Environment(
            core: { try bridge.smartAlerts() },
            storedSettings: { Data(settings.utf8) },
            deviceID: { key in modelDevice.runtime(key)?.deviceID },
            secrets: MemorySecrets([:]))
        let stub = StubDetector(person: true)
        stub.classID = 2  // car
        let handle = DetectorHandle(service: DetectorService(detector: stub), labels: DetectorProvider.cocoLabels)
        let controller = SmartAlertsController(
            environment: environment, installer: ModelInstaller(manifest: nil, locate: { nil }), notifier: notifier,
            detector: { handle })
        model.smartAlerts = controller
        controller.configure()
        guard await wait(30, { if case .running = controller.state { true } else { false } }) else {
            return fail(logsSecond, "the core was not configured (\(controller.state))")
        }
        guard let core = try? bridge.smartAlerts() else { return fail(logsSecond, "no core") }
        let cam = #"{"device":\#(deviceID),"channel":\#(channel)}"#

        // Decision of one trigger, as the log answers it.
        func decided(_ trigger: UInt64) async -> [String: Any]? {
            var found: [String: Any]?
            _ = await wait(60) {
                guard let text = try? core.alertsGet(query: #"{"trigger_id":\#(trigger)}"#),
                    let o = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any],
                    let d = ((o["entries"] as? [[String: Any]])?.first)?["decision"] as? [String: Any]
                else { return false }
                found = d
                return true
            }
            return found
        }
        func put(_ kind: String) -> UInt64? {
            try? core.alertsTriggerPut(json: #"{"source":"device","camera":\#(cam),"kind":"\#(kind)","state":"start"}"#)
        }

        // alerts-cooldown-logs-second: two vehicles, one after the other.
        guard let t1 = put("vehicle"), let first = await decided(t1) else { return fail(logsSecond, "no decision for the first vehicle") }
        guard first["decision"] as? String == "alert", first["quieted"] == nil else {
            return fail(logsSecond, "the first vehicle is \(first["decision"] ?? "?") \(first["quieted"] ?? "")")
        }
        guard await wait(15, { notifier.notifications.count == 1 }) else { return fail(logsSecond, "the first vehicle did not notify") }
        guard let t2 = put("vehicle"), let second = await decided(t2) else { return fail(logsSecond, "no decision for the second vehicle") }
        let why = (second["quieted"] as? [String: Any])?["why"] as? String
        if second["decision"] as? String != "alert" || why != "cooldown" || second["alert_id"] as? Int != first["alert_id"] as? Int {
            fail(logsSecond, "the second vehicle is \(second["decision"] ?? "?"), quieted \(why ?? "no"), alert \(second["alert_id"] ?? "?")")
        }
        let alerts = model.alertsModel!
        var sets: [AlertEvidence] = []
        let kept = await wait(20) {
            sets = [t1, t2].compactMap { alerts.evidence(trigger: $0) }
            return sets.count == 2 && sets.allSatisfy { $0.decision == "alert" && !$0.frames.isEmpty }
        }
        if !kept { fail(logsSecond, "evidence kept for \(sets.count) of 2 detections") }
        // Nothing more happens for the quieted one: a fixed short wait proves
        // that no second notification follows.
        try? await Task.sleep(for: .milliseconds(400))
        if notifier.notifications.count != 1 { fail(logsSecond, "\(notifier.notifications.count) notifications for two vehicles, not one") }
        print("\(logsSecond): two vehicles logged with evidence, the second quieted by the cooldown, one notification")

        // alerts-night-evidence-expiry: the night is the whole day, so the set
        // lasts until the next 06:00 local plus 6 h, not the 30 minutes.
        if let set = sets.first {
            let created = Date(timeIntervalSince1970: Double(set.createdMs) / 1000)
            let morning = Calendar.current.nextDate(
                after: created, matching: DateComponents(hour: 6, minute: 0, second: 0), matchingPolicy: .nextTime)
            let want = (morning?.timeIntervalSince1970 ?? 0) * 1000 + 6 * 3_600_000
            let got = Double(set.expiresMs ?? 0)
            if abs(got - want) > 120_000 || got - Double(set.createdMs) < 3_600_000 {
                fail(expiry, "the night set expires at \(Int64(got)), expected \(Int64(want)) (created \(set.createdMs))")
            } else {
                print("\(expiry): a night set made now is kept \(Int((got - Double(set.createdMs)) / 3_600_000)) h, past the 30 minute retention")
            }
        } else {
            fail(expiry, "no evidence set to check")
        }

        // alerts-cooldown-always-alert: a person on an outside camera at night
        // alerts as before, even inside its own cooldown (an update of the
        // same alert, not a quieted one).
        stub.classID = 0
        guard let p1 = put("people"), let firstPerson = await decided(p1) else { return fail(always, "no decision for the first person") }
        guard let p2 = put("people"), let secondPerson = await decided(p2) else { return fail(always, "no decision for the second person") }
        if firstPerson["always"] as? String != "night_outside_person" { fail(always, "the first person is not an always-alert case: \(firstPerson["always"] ?? "-")") }
        if secondPerson["decision"] as? String != "alert" || secondPerson["quieted"] != nil
            || secondPerson["always"] as? String != "night_outside_person" || secondPerson["alert_id"] as? Int != firstPerson["alert_id"] as? Int
        {
            fail(always, "the second person is \(secondPerson["decision"] ?? "?"), quieted \(secondPerson["quieted"] ?? "no"), always \(secondPerson["always"] ?? "-")")
        } else {
            print("\(always): the second person inside the cooldown still alerted (revision \(secondPerson["rev"] ?? "?"))")
        }
    }

    /// Evidence for an outdoor person check is kept with its decision and
    /// the detector's boxes (step `alerts-evidence-kept`).
    private func evidenceKept(trigger: UInt64, decision: String, boxes: Bool) async {
        let id = "alerts-evidence-kept"
        let alerts = model.alertsModel!
        var set: AlertEvidence?
        let found = await wait(15) {
            set = alerts.evidence(trigger: trigger)
            return set?.decision != nil
        }
        guard found, let set else { return fail(id, "no evidence was kept for the \(decision) check of trigger \(trigger)") }
        if set.decision != decision { fail(id, "the evidence holds a \(set.decision ?? "?") decision, not \(decision)") }
        let drawn = set.method("detector")?.boxes.values.flatMap { $0 }.count
        if drawn == nil || (boxes && drawn == 0) || (!boxes && drawn != 0) {
            fail(id, "the detector's boxes in the evidence are \(drawn.map(String.init) ?? "missing")")
        }
        guard let frame = set.frames.first, alerts.evidenceFrame(trigger: trigger, index: frame.index) != nil else {
            return fail(id, "the evidence has no readable frame")
        }
        if set.expiresMs == nil || set.pinned { fail(id, "the evidence has no expiry") }
        print("\(id): kept \(set.frames.count) frame(s) and the detector's \(drawn ?? 0) box(es) with the \(decision) decision")
    }

    /// A motion-only decision keeps no evidence (the other half of
    /// `alerts-evidence-kept`).
    private func motionOnlyKeepsNothing(device: DeviceRuntime, channel: Int) async {
        let id = "alerts-evidence-kept"
        guard let core = try? model.bridge.smartAlerts(), let deviceID = device.deviceID,
            let trigger = try? core.alertsTriggerPut(
                json: #"{"source":"device","camera":{"device":\#(deviceID),"channel":\#(channel)},"kind":"motion","state":"start"}"#)
        else { return fail(id, "the motion trigger was refused") }
        let decided = await wait(30) {
            (try? core.alertsGet(query: #"{"trigger_id":\#(trigger)}"#)).map { $0.contains("\"decision\"") && !$0.contains("\"entries\":[]") }
                ?? false
        }
        guard decided else { return fail(id, "no decision was made for the motion trigger") }
        if model.alertsModel.evidence(trigger: trigger) != nil { fail(id, "a motion-only decision kept evidence") }
        print("\(id): a motion-only decision kept none")
    }

    /// The debugging and review steps on the UI scenario's settings: a second
    /// sensor on a local broker (messages), the closed port of the first
    /// (diagnostics), simulated triggers, the Home Assistant test alert, and
    /// the Events window's overlay.
    private func reviewSteps(
        alerts: SmartAlertsModel, core: any CoreAPI, ha: MockHomeAssistant, events: SmartAlertEventsModel,
        stub: StubDetector, controller: SmartAlertsController, shown: SmartAlertLogEntry, sensor: String, camera: CameraRef,
        other: CameraRef?
    ) async {
        // alerts-sensor-diagnostics: the first sensor's broker port is closed.
        var diag = "alerts-sensor-diagnostics"
        let refused = await wait(30) {
            alerts.refreshStatus()
            return SensorConnection.find(id: sensor, in: alerts.status)?.errorKind == "refused"
        }
        let line = alerts.sensorStatus(0) ?? ""
        if !refused || !line.contains("Connection refused") || !line.contains("Never connected") {
            fail(diag, "the sensor's status line is \"\(line)\"")
        } else {
            print("\(diag): \"\(line)\"")
        }

        // alerts-sensor-messages: a second sensor on a local broker.
        let messages = "alerts-sensor-messages"
        guard let broker = MockMQTTBroker() else { return fail(messages, "no local broker") }
        defer { broker.stop() }
        alerts.update { s in
            var m = MQTTSensor(key: "sensor-msg", name: "Sensor Messages")
            m.host = "127.0.0.1"
            m.port = Int(broker.port)
            m.topic = "example/sensors/messages"
            s.sensors.append(m)
        }
        guard await wait(30, { broker.subscribers == 1 }) else { return fail(messages, "the sensor never subscribed to the broker") }
        broker.publish(topic: "example/sensors/messages", payload: Data("ON".utf8))
        broker.publish(topic: "example/sensors/messages", payload: Data("example-garbage".utf8))
        var page: SensorMessagePage?
        let arrived = await wait(15) {
            page = alerts.sensorMessages(key: "sensor-msg", after: 0)
            return (page?.entries.count ?? 0) == 2
        }
        guard arrived, let entries = page?.entries else { return fail(messages, "the messages did not arrive (\(page?.entries.count ?? -1))") }
        if entries.map(\.outcomeCode) != ["matched", "not_a_trigger_value"]
            || entries[0].outcomeText != "matched sensor sensor-msg as sensor_motion (start)"
            || entries[1].payload != "example-garbage"
        {
            fail(messages, "the outcomes are \(entries.map { "\($0.outcomeCode): \($0.outcomeText)" })")
        }
        // Memory only: the payload is in no event and no saved file.
        if (try? core.alertsGet(query: "{}"))?.contains("example-garbage") == true { fail(messages, "a payload reached the alert log") }
        let window = SensorMessagesModel(app: model, key: "sensor-msg")
        window.refresh()
        if window.shown.count != 2 || window.connection?.hasPrefix("Connected") != true {
            fail(messages, "the messages view shows \(window.shown.count) messages, connection \(window.connection ?? "-")")
        }
        window.filter = .matched
        if window.shown.count != 1 { fail(messages, "the outcome filter shows \(window.shown.count)") }
        window.clear()
        window.stop()
        if alerts.sensorMessages(key: "sensor-msg", after: 0)?.entries.isEmpty != true { fail(messages, "Clear left messages") }
        print("\(messages): matched and ignored messages listed with their outcomes, filtered, cleared")

        // alerts-ha-test-alert: a test alert through Home Assistant.
        let test = "alerts-ha-test-alert"
        let beforeTest = ha.turnOns(Self.helper)
        alerts.sendHomeAssistantTestAlert()
        let delivered = await wait(30) { alerts.lastOutputTest != nil }
        let result = alerts.lastOutputTest
        if !delivered || result?.ok != true || result?.httpStatus != 200 || !(alerts.homeAssistantSend ?? "").hasPrefix("Delivered (HTTP 200)")
            || ha.turnOns(Self.helper) != beforeTest + 1
        {
            fail(test, "result \(String(describing: result)), line \(alerts.homeAssistantSend ?? "-"), pulses \(ha.turnOns(Self.helper) - beforeTest)")
        } else {
            print("\(test): \"\(alerts.homeAssistantSend ?? "")\"")
        }

        // alerts-simulate-trigger: a vehicle on the sensor's camera (the person
        // alert above holds that camera's person cooldown), then a person
        // on another camera that raised no alert, ticked for Home Assistant.
        let sim = "alerts-simulate-trigger"
        let beforeSim = ha.turnOns(Self.helper)
        stub.classID = 2  // car
        guard let first = alerts.simulate(sensor: sensor, sendToHomeAssistant: false) else { return fail(sim, "the trigger was refused") }
        var entry: SmartAlertLogEntry?
        let decided = await wait(40) {
            guard let text = try? core.alertsGet(query: #"{"trigger_id":\#(first)}"#),
                let page = SmartAlertLogPage.parse(text)
            else { return false }
            entry = page.entries.first
            return entry != nil
        }
        guard decided, let entry else { return fail(sim, "no test decision was logged") }
        if !entry.isTest || entry.category != "test" || entry.decision != "alert" {
            fail(sim, "the decision is \(entry.category)/\(entry.decision), test \(entry.isTest)")
        }
        if !SmartAlertEventsView.decisionLabel(entry).hasPrefix("Test") { fail(sim, "the Events window does not mark it a test") }
        // A pulse would start within milliseconds of the decision: a short
        // fixed wait proves that none comes.
        try? await Task.sleep(for: .milliseconds(400))
        if ha.turnOns(Self.helper) != beforeSim { fail(sim, "a simulated trigger reached Home Assistant unticked") }
        guard let other else { return print("\(sim): the ticked half is skipped (the device has fewer than three channels)") }
        let applied = controller.configureCount
        alerts.update { s in
            s.setCamera(AlertCameraSettings(camera: other))
            if let i = s.sensors.firstIndex(where: { $0.key == "sensor-msg" }) { s.sensors[i].cameras = [SensorCamera(camera: other)] }
        }
        guard await wait(30, { controller.configureCount > applied }) else { return fail(sim, "the core did not take the new mapping") }
        stub.classID = 0  // person
        guard let second = alerts.simulate(sensor: "sensor-msg", sendToHomeAssistant: true) else { return fail(sim, "the ticked trigger was refused") }
        guard await wait(60, { ha.turnOns(Self.helper) == beforeSim + 1 }) else {
            let why = (try? core.alertsGet(query: #"{"trigger_id":\#(second)}"#)).flatMap(SmartAlertLogPage.parse)?.entries.first
            return fail(
                sim,
                "the ticked simulated trigger did not reach Home Assistant: decision \(why?.decision ?? "none") \(why?.reason ?? "") test \(why?.isTest ?? false); \(ha.log.suffix(3))"
            )
        }
        print("\(sim): a test decision with no Home Assistant delivery, then one with it when ticked")

        // alerts-events-overlay: the Events window's detail pane for the
        // sensor's first (person) alert shows the kept frame with its boxes.
        let overlay = "alerts-events-overlay"
        events.reload()
        events.select(shown.id)
        guard let set = events.evidence, let image = events.evidenceImages[events.frame] else {
            return fail(overlay, "the Events window loaded no kept picture for the alert")
        }
        let drawn = EvidenceOverlay.layout(
            set, frame: events.frame, visible: events.visibleMethods, picture: image.size, in: CGSize(width: 400, height: 240))
        let pictureRect = EvidenceOverlay.pictureRect(picture: image.size, in: CGSize(width: 400, height: 240))
        if drawn.isEmpty || !drawn.allSatisfy({ pictureRect.insetBy(dx: -0.5, dy: -0.5).contains($0.rect) }) {
            fail(overlay, "\(drawn.count) boxes, inside the picture: \(drawn.map { pictureRect.contains($0.rect) })")
        }
        events.toggleMethod("detector")
        if !EvidenceOverlay.layout(set, frame: events.frame, visible: events.visibleMethods, picture: image.size, in: CGSize(width: 400, height: 240)).isEmpty {
            fail(overlay, "switching the method off left its boxes")
        }
        events.keepEvidence()
        if events.evidence?.pinned != true { fail(overlay, "Keep did not pin the set") }
        print("\(overlay): \(drawn.count) box(es) drawn over the kept picture, toggled off, kept")
    }

    /// Polls `condition` (about every 20 ms) until it holds or `seconds` pass.
    private func wait(_ seconds: Double, _ condition: @MainActor () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(20))
        }
        return condition()
    }
}

/// A detector for the mock's synthetic pictures: a person walking across
/// every crop, or nothing.
private final class StubDetector: CropDetecting, @unchecked Sendable {
    let person: Bool
    private let cls = Mutex(0)
    private let calls = Mutex(0)
    init(person: Bool) { self.person = person }
    /// The model class it reports (0 person, 2 car, 16 dog).
    var classID: Int {
        get { cls.withLock { $0 } }
        set { cls.withLock { $0 = newValue } }
    }

    /// The box moves by half its width on every call, so that the core's
    /// moving-versus-parked and repeat filters see something walking past
    /// rather than a stationary object.
    func detect(picture: DetectorPicture, crops: [NormRect]) throws -> [CropDetections] {
        let step = calls.withLock { n in
            defer { n += 1 }
            return n % 12
        }
        let x = 0.1 + 0.05 * Double(step)
        return crops.map { crop in
            CropDetections(
                crop: crop,
                detections: person
                    ? [RawDetection(classID: classID, score: 0.9, box: NormRect(x: x, y: 0.3, width: 0.1, height: 0.4))]
                    : [])
        }
    }
}
