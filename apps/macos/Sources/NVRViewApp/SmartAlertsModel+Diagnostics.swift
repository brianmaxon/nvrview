import AppKit
import Foundation
import NVRCore

// Debugging and reviewing smart alerts on the app's side (docs/UX.md "Smart
// alerts", docs/NOTIFICATIONS.md "Diagnostics"): a sensor's message log,
// simulated triggers, a test alert through Home Assistant, and the kept
// evidence of checks.

extension SmartAlertsModel {
    // MARK: Sensor messages

    /// The messages that came after `after` (the next sequence number the
    /// caller holds), nil when the core cannot say.
    func sensorMessages(key: String, after: Int64) -> SensorMessagePage? {
        guard
            let text = try? bridge.smartAlertsAPI().alertsMessages(
                request: SensorMessagePage.request(input: key, after: after))
        else { return nil }
        return SensorMessagePage.parse(text)
    }

    /// Empties the sensor's log in the core.
    @discardableResult
    func clearSensorMessages(key: String) -> Bool {
        (try? bridge.smartAlertsAPI().alertsMessages(
            request: SensorMessagePage.request(input: key, after: 0, limit: 1, clear: true))) != nil
    }

    // MARK: Simulate trigger

    /// Runs the whole chain as if the sensor fired (all its cameras, or just
    /// `camera`). The decision is marked as a test; Home Assistant gets it
    /// only with `sendToHomeAssistant`.
    @discardableResult
    func simulate(sensor key: String, camera: AlertCamera? = nil, sendToHomeAssistant: Bool = false) -> UInt64? {
        guard let sensor = settings.sensors.first(where: { $0.key == key }) else { return nil }
        var sent: UInt64?
        var o: [String: Any] = [
            "source": "external", "origin": key, "kind": sensor.triggerKind, "state": "pulse", "test": true,
            "send_to_home_assistant": sendToHomeAssistant,
        ]
        if let camera { o["only_camera"] = ["device": camera.device, "channel": camera.channel, "lens": camera.lens] }
        let data = (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys])) ?? Data("{}".utf8)
        do throws(CoreStatus) {
            let id = try bridge.smartAlertsAPI().alertsTriggerPut(json: String(decoding: data, as: UTF8.self))
            sent = id
            simulateNote =
                "Simulated trigger \(id) sent\(camera.map { " for \(cameraName($0))" } ?? ""). "
                + "The decision appears in Smart Alert Events marked Test"
                + (sendToHomeAssistant ? "; Home Assistant gets it too." : "; Home Assistant does not.")
        } catch .notConfigured {
            simulateNote = "Smart alerts are off or not set up yet."
        } catch .unsupported {
            simulateNote = "This core cannot simulate a trigger."
        } catch {
            simulateNote = "The trigger was not taken (\(error))."
        }
        changed()
        return sent
    }

    func cameraName(_ c: AlertCamera) -> String { model.cameraRef(c).map(model.name(of:)) ?? "Camera \(c.channel + 1)" }

    // MARK: Send test alert

    /// The Home Assistant output's position in the core's `outputs` (after
    /// the desktop one).
    static let homeAssistantOutputIndex = 1

    /// Delivers one test alert through the configured Home Assistant output
    /// (a pulse of its rules' entities); the result arrives as an event.
    func sendHomeAssistantTestAlert() {
        homeAssistantSend = "Sending…"
        do throws(CoreStatus) {
            try bridge.smartAlertsAPI().alertsOutputTest(request: #"{"output":\#(Self.homeAssistantOutputIndex)}"#)
        } catch .notConfigured {
            homeAssistantSend = "Smart alerts are off or not set up yet."
        } catch .unknownID {
            homeAssistantSend = "Home Assistant is not set up in the core (save the address and token first)."
        } catch {
            homeAssistantSend = "The test alert was not sent (\(error))."
        }
        changed()
    }

    // MARK: Evidence

    func evidenceList(sinceMs: Int64? = nil) -> EvidenceList? {
        var q: [String: Any] = [:]
        if let sinceMs { q["since_ms"] = sinceMs }
        let data = (try? JSONSerialization.data(withJSONObject: q, options: [.sortedKeys])) ?? Data("{}".utf8)
        guard let text = try? bridge.smartAlertsAPI().alertEvidenceGet(query: String(decoding: data, as: UTF8.self))
        else { return nil }
        return EvidenceList.parse(text)
    }

    func evidence(trigger: UInt64) -> AlertEvidence? {
        guard let text = try? bridge.smartAlertsAPI().alertEvidenceGet(query: #"{"trigger_id":\#(trigger)}"#),
            let json = JSONValue.parse(text)
        else { return nil }
        return AlertEvidence(json)
    }

    func evidenceFrame(trigger: UInt64, index: Int) -> NSImage? {
        guard let data = try? bridge.smartAlertsAPI().alertEvidenceFrameGet(triggerID: trigger, index: UInt32(index))
        else { return nil }
        return NSImage(data: data)
    }

    @discardableResult
    func pinEvidence(trigger: UInt64) -> Bool {
        (try? bridge.smartAlertsAPI().alertEvidencePin(triggerID: trigger)) != nil
    }
}
