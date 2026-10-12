import AlertHost
import DetectorKit
import Foundation
import NVRCore

/// Self-test steps `alerts-e2e-verified` and `alerts-e2e-not-confirmed`
/// (docs/NOTIFICATIONS.md): smart alerts end to end on the real core against
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
/// that finds nothing. Runs only on the real core, against a device on this
/// computer.
@MainActor
final class AlertsSelfTest {
    private let model: AppModel
    private var problems: [String] = []
    private static let helper = "input_boolean.example_alert"
    private static let token = "example-token-0123456789"

    init(model: AppModel) { self.model = model }

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
        return problems
    }

    private func fail(_ id: String, _ text: String) { problems.append("\(id): \(text)") }

    private func scenario(
        _ id: String, device: AppModel.DeviceRuntime, channel: Int, ha: MockHomeAssistant, personVisible: Bool,
        expectAlert: Bool
    ) async {
        let before = ha.turnOns(Self.helper)
        let camera = #"{"device_key":"\#(device.key)","channel":\#(channel)}"#
        let settings = """
            {"enabled":true,"cameras":[{"camera":\(camera)}],
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
            if note.snapshot?.starts(with: [0xFF, 0xD8]) != true { fail(id, "the notification has no snapshot") }
            if let alertID = decision["alert_id"] as? Int,
                (try? core.alertSnapshotGet(alertID: UInt64(alertID)))?.starts(with: [0xFF, 0xD8]) != true
            {
                fail(id, "the core did not keep the snapshot")
            }
            print("\(id): pulsed Home Assistant, notified \"\(note.title)\", snapshot \(note.snapshot?.count ?? 0) bytes")
        } else {
            guard kind == "quiet" else { return fail(id, "expected a quiet decision, got \(kind)") }
            // Proving that nothing happens takes a short fixed wait: a pulse
            // would start within milliseconds of the decision.
            try? await Task.sleep(for: .milliseconds(400))
            if ha.turnOns(Self.helper) != before { fail(id, "Home Assistant was pulsed for an unconfirmed trigger") }
            if !notifier.notifications.isEmpty { fail(id, "a notification was raised for an unconfirmed trigger") }
            print("\(id): quiet, no pulse, no notification")
        }
        let configured = controller.lastConfigured ?? ""
        if configured.contains(Self.token) { fail(id, "the token appeared in an event") }
        if let text = try? core.alertsGet(query: "{}"), text.contains(Self.token) { fail(id, "the token appeared in the alert log") }
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

/// A detector for the mock's synthetic pictures: a person in the middle of
/// every crop, or nothing.
private final class StubDetector: CropDetecting, @unchecked Sendable {
    let person: Bool
    init(person: Bool) { self.person = person }

    func detect(picture: DetectorPicture, crops: [NormRect]) throws -> [CropDetections] {
        crops.map { crop in
            CropDetections(
                crop: crop,
                detections: person
                    ? [RawDetection(classID: 0, score: 0.9, box: NormRect(x: 0.4, y: 0.3, width: 0.1, height: 0.4))]
                    : [])
        }
    }
}
