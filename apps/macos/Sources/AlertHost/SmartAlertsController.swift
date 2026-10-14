import DetectorKit
import Foundation
import NVRCore

/// The app's side of smart alerts (docs/NOTIFICATIONS.md): configures the
/// core from the user's `alerts.json` with the Keychain's credentials,
/// fetches the detector model on first use, answers verification requests
/// off the main thread, makes the snapshot of each alert, and raises the
/// desktop notification. Feed it the core's `smartAlert` events.
///
/// Smart alerts are off unless the settings document exists and says
/// `"enabled": true`. Nothing here is a model of the rules: the core decides.
@MainActor
public final class SmartAlertsController {
    /// What the app tells the controller about its surroundings.
    public struct Environment {
        public var core: @Sendable () throws -> any CoreAPI
        /// The stored settings document (`alerts.json`), if there is one.
        public var storedSettings: @MainActor () -> Data?
        /// This run's core device ID for a device's stable key.
        public var deviceID: @MainActor (String) -> UInt64?
        /// The user's update source (the model is fetched from its folder).
        public var updateSource: @MainActor () -> String?
        public var utcOffsetMinutes: @MainActor () -> Int
        public var secrets: any AlertSecrets

        public init(
            core: @escaping @Sendable () throws -> any CoreAPI,
            storedSettings: @escaping @MainActor () -> Data?,
            deviceID: @escaping @MainActor (String) -> UInt64?,
            updateSource: @escaping @MainActor () -> String? = { nil },
            utcOffsetMinutes: @escaping @MainActor () -> Int = { TimeZone.current.secondsFromGMT() / 60 },
            secrets: any AlertSecrets
        ) {
            self.core = core
            self.storedSettings = storedSettings
            self.deviceID = deviceID
            self.updateSource = updateSource
            self.utcOffsetMinutes = utcOffsetMinutes
            self.secrets = secrets
        }
    }

    public enum State: Equatable, Sendable {
        /// No settings, or they say smart alerts are off.
        case off
        case starting
        /// The core was configured (with what it clamped).
        case running(issues: [String])
        case failed(String)
    }

    public private(set) var state: State = .off
    public private(set) var modelStatus: ModelStatus = .checking
    /// Alerts raised so far, for the status line and tests.
    public private(set) var alertsSeen = 0
    /// The `alerts_configured` event's JSON, as the core sent it.
    public private(set) var lastConfigured: String?
    /// How many times the core took the settings (for tests that wait for
    /// a change to be applied).
    public private(set) var configureCount = 0

    private let environment: Environment
    private let installer: ModelInstaller
    private let notifier: any AlertNotifying
    private let verifier: AlertVerifier
    private var desktop = DesktopOutput(enabled: true, minimum: 1)
    private var generation = 0
    /// Verification requests being worked on (answered or skipped when it is
    /// zero again), for tests that wait on it.
    public private(set) var inFlight = 0

    public init(
        environment: Environment, installer: ModelInstaller, notifier: any AlertNotifying,
        detector: (@Sendable () async -> DetectorHandle?)? = nil
    ) {
        self.environment = environment
        self.installer = installer
        self.notifier = notifier
        let provider = DetectorProvider(installer: installer)
        verifier = AlertVerifier(
            core: environment.core, detector: detector ?? { await provider.detector() })
    }

    // MARK: Configuring

    /// Reads the stored settings and configures the core; when smart alerts
    /// are enabled the model is fetched first if it is missing and a source
    /// is set. Call at startup (after the devices are added) and whenever
    /// the settings or the devices change; inputs and outputs whose entry
    /// and credential did not change keep running.
    public func configure() {
        generation += 1
        let mine = generation
        guard let stored = environment.storedSettings() else {
            state = .off
            return
        }
        state = .starting
        Task { [weak self] in
            guard let self else { return }
            // The model first, so no check starts unanswered while it downloads.
            let preview = try? AlertSettingsDocument.build(
                stored: stored, deviceID: { _ in nil }, utcOffsetMinutes: 0, secrets: MemorySecrets())
            if preview?.enabled == true {
                await self.refreshModel()
            }
            guard mine == self.generation else { return }
            self.send(stored: stored)
        }
    }

    private func refreshModel() async {
        modelStatus = .downloading
        let source = environment.updateSource()
        let status = await installer.ensure(updateSource: source)
        modelStatus = status
        _ = await notifier.authorize()
    }

    private func send(stored: Data) {
        do {
            let built = try AlertSettingsDocument.build(
                stored: stored, deviceID: { environment.deviceID($0) },
                utcOffsetMinutes: environment.utcOffsetMinutes(), secrets: environment.secrets)
            desktop = DesktopOutput(document: built.json)
            verifier.evidence.setInside(Self.insideCameras(in: built.json))
            guard built.enabled || state != .off else {
                state = .off
                return
            }
            try environment.core().alertsConfigure(json: built.json)
            configureCount += 1
            state = built.enabled ? .running(issues: []) : .off
        } catch let error as AlertSettingsError {
            state = .failed(error == .notAnObject ? "alerts.json is not a settings object." : "alerts.json is unusable.")
        } catch {
            state = .failed("The core refused the smart-alert settings.")
        }
    }

    // MARK: Events

    /// Feeds one `smartAlert` event from the core.
    public func handle(_ event: SmartAlertEvent) {
        switch event {
        case .verifyRequest(let request):
            guard state != .off else { return }
            inFlight += 1
            let verifier = verifier
            Task.detached { [weak self] in
                await verifier.answer(request)
                await self?.finished()
            }
        case .alert(let state, let json):
            guard state == "new" || state == "update" else { return }
            alertsSeen += (state == "new") ? 1 : 0
            raise(json: json, isNew: state == "new")
        case .configured(let json):
            lastConfigured = json
            if case .running = self.state, let issues = Self.issues(in: json) { self.state = .running(issues: issues) }
        case .verifyRequestDone, .verifyStep, .triggerRejected, .outputTest, .outputTargets:
            break
        }
    }

    private func finished() { inFlight -= 1 }

    /// The cameras the settings mark as inside a building: their checks leave
    /// no evidence.
    static func insideCameras(in json: String) -> Set<AlertCamera> {
        guard let data = json.data(using: .utf8),
            let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
            let cameras = object["cameras"] as? [[String: Any]]
        else { return [] }
        var out: Set<AlertCamera> = []
        for entry in cameras where (entry["exposure"] as? String) == "inside" {
            if let c = entry["camera"] as? [String: Any], let device = (c["device"] as? NSNumber)?.uint64Value,
                let channel = (c["channel"] as? NSNumber)?.uint16Value
            {
                out.insert(AlertCamera(device: device, channel: channel, lens: (c["lens"] as? NSNumber)?.uint8Value ?? 0))
            }
        }
        return out
    }

    private static func issues(in json: String) -> [String]? {
        guard let data = json.data(using: .utf8),
            let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { return nil }
        return object["issues"] as? [String]
    }

    /// The snapshot, then the notification.
    private func raise(json: String, isNew: Bool) {
        guard let alert = AlertSummary(eventJSON: json) else { return }
        let desktop = desktop
        let verifier = verifier
        let notifier = notifier
        let core = environment.core
        Task.detached {
            var jpeg: Data?
            if let camera = alert.camera, let picture = verifier.pictures.latest(for: camera) {
                jpeg = SnapshotMaker.jpeg(of: picture)
                if let jpeg { try? core().alertSnapshotPut(alertID: alert.alertID, data: jpeg) }
            }
            guard desktop.accepts(severity: alert.severity), isNew else { return }
            await notifier.post(
                AlertNotification(
                    alertID: alert.alertID, title: alert.title, body: alert.explanation,
                    critical: alert.severity == "critical", snapshot: jpeg, thread: alert.thread))
        }
    }

    /// Waits until the verification requests handed out so far are done.
    public func idle() async {
        while inFlight > 0 { await Task.yield() }
    }
}

/// The fields of an `alert` event the desktop notification needs.
struct AlertSummary: Sendable, Equatable {
    var alertID: UInt64
    var severity: String
    var className: String
    var explanation: String
    var camera: AlertCamera?
    /// A simulated trigger: the notification says so.
    var isTest = false

    init?(eventJSON: String) {
        guard let data = eventJSON.data(using: .utf8),
            let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
            let decision = object["decision"] as? [String: Any],
            (decision["decision"] as? String) == "alert",
            let id = (decision["alert_id"] as? NSNumber)?.uint64Value
        else { return nil }
        alertID = id
        isTest = (object["test"] as? Bool) ?? false
        severity = (decision["severity"] as? String) ?? "normal"
        className = (decision["class"] as? String) ?? "other"
        explanation = (decision["explanation"] as? String) ?? ""
        if let c = decision["camera"] as? [String: Any],
            let device = (c["device"] as? NSNumber)?.uint64Value,
            let channel = (c["channel"] as? NSNumber)?.uint16Value
        {
            camera = AlertCamera(device: device, channel: channel, lens: (c["lens"] as? NSNumber)?.uint8Value ?? 0)
        }
    }

    /// "Critical person alert", "Vehicle alert", ...
    var title: String {
        let noun: String
        switch className {
        case "smoke": noun = "smoke alarm"
        case "co": noun = "carbon monoxide alarm"
        case "visitor": noun = "doorbell"
        default: noun = "\(className) alert"
        }
        let text = severity == "critical" ? "Critical \(noun)" : noun
        let title = text.prefix(1).uppercased() + text.dropFirst()
        return isTest ? "Test: \(title)" : title
    }

    var thread: String {
        camera.map { "camera-\($0.device)-\($0.channel)-\($0.lens)" } ?? "alerts"
    }
}

/// Whether the desktop output is on and the lowest severity it takes.
struct DesktopOutput: Sendable, Equatable {
    var enabled: Bool
    /// 0 low, 1 normal, 2 high, 3 critical.
    var minimum: Int

    init(enabled: Bool, minimum: Int) {
        self.enabled = enabled
        self.minimum = minimum
    }

    /// From the built settings document: on unless `outputs` is listed
    /// without an enabled `desktop` entry.
    init(document json: String) {
        guard let data = json.data(using: .utf8),
            let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
            let outputs = object["outputs"] as? [[String: Any]]
        else {
            self.init(enabled: true, minimum: 1)
            return
        }
        let entry = outputs.first { ($0["kind"] as? String) == "desktop" && ($0["enabled"] as? Bool) != false }
        self.init(enabled: entry != nil, minimum: Self.rank(entry?["min_severity"] as? String ?? "normal"))
    }

    static func rank(_ severity: String) -> Int {
        ["low": 0, "normal": 1, "high": 2, "critical": 3][severity] ?? 1
    }

    func accepts(severity: String) -> Bool { enabled && Self.rank(severity) >= minimum }
}
