import AppKit
import Foundation
import NVRCore
import VideoKit

// Smart alerts on the app's side (docs/UX.md "Smart alerts",
// docs/NOTIFICATIONS.md): the settings (a file of their own; secrets in the
// Keychain only), sending them to the core, the live checks the views draw,
// the event log's calls, and the status lines. One per app, owned by
// `AppModel`.
//
// For the other app-side parts (the detector, desktop notifications), the
// seams are: `settings` (what the user chose), `secret(_:)` (the Keychain
// items), `request(_:)` (a `verify_request` by key), `recordDetections`
// (the boxes the detector answered, so the live view draws them), and
// `subscribeEvents` (every smart-alert event, after the views took it).

/// The settings file: one JSON document, written atomically; in memory only
/// when `file` is nil. Beside it, `stored` gets the document's stored form
/// (`alerts.json`: cameras by device key, no secrets), the file a host that
/// configures the core itself reads (docs/NOTIFICATIONS.md).
struct SmartAlertSettingsFile {
    let file: URL?
    var stored: URL?

    init(file: URL?, stored: URL? = nil) {
        self.file = file
        self.stored = stored
    }

    /// Writes the stored form when it changed.
    func saveStored(_ data: Data) {
        guard let stored, (try? Data(contentsOf: stored)) != data else { return }
        try? FileManager.default.createDirectory(at: stored.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: stored, options: .atomic)
    }

    func load() -> SmartAlertSettings {
        guard let file, let data = try? Data(contentsOf: file) else { return SmartAlertSettings() }
        return SmartAlertSettings.fromFile(data)
    }

    func save(_ s: SmartAlertSettings) {
        guard let file else { return }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? s.fileData().write(to: file, options: .atomic)
    }
}

@MainActor
@Observable
final class SmartAlertsModel {
    @ObservationIgnored private unowned let model: AppModel
    @ObservationIgnored private let bridge: CoreBridge
    @ObservationIgnored private let credentials: any CredentialStore
    @ObservationIgnored private let file: SmartAlertSettingsFile

    /// What the user chose (saved on every `update`).
    private(set) var settings: SmartAlertSettings
    /// The live checks (shared by every window).
    private(set) var board = VerificationBoard()
    /// The core's answer to the last settings sent (`alerts_configured`).
    private(set) var configured: JSONValue?
    /// The service's status from the last log query (`inputs`, `outputs`,
    /// `muted`, ...).
    private(set) var status: JSONValue?
    /// Why the settings could not be sent, if they could not.
    private(set) var sendError: String?
    /// The last delivery result per output index (from `delivery` steps).
    private(set) var lastDelivery: [Int: (ok: Bool, at: Date)] = [:]
    /// The last Home Assistant connection test, in words.
    var homeAssistantTest: String?
    /// Requests seen, by key (until done), for the detector and the boxes.
    @ObservationIgnored private var requests: [String: VerifyRequest] = [:]
    /// The latest checked picture per check and angle (window mode).
    @ObservationIgnored private(set) var pictures: [UInt64: [AlertCamera: CGImage]] = [:]
    @ObservationIgnored private var listeners: [UUID: () -> Void] = [:]
    @ObservationIgnored private var eventListeners: [UUID: (SmartAlertEvent) -> Void] = [:]
    /// The device IDs the last document was built with.
    @ObservationIgnored private var sentDevices: [String: UInt64] = [:]
    @ObservationIgnored private var sentOffset: Int?
    /// Whether a camera shows on any viewer window (set by the app
    /// delegate; the window mode needs to know).
    @ObservationIgnored var isOnScreen: (CameraRef) -> Bool = { _ in false }
    /// Shows or hides the verification window (set by the app delegate).
    @ObservationIgnored var showPanel: (Bool) -> Void = { _ in }

    init(model: AppModel, bridge: CoreBridge, credentials: any CredentialStore, file: SmartAlertSettingsFile) {
        self.model = model
        self.bridge = bridge
        self.credentials = credentials
        self.file = file
        settings = file.load()
        board.settings = settings.view
    }

    // MARK: Settings

    /// Changes the settings: saved, then sent to the core.
    func update(_ body: (inout SmartAlertSettings) -> Void) {
        var s = settings
        body(&s)
        guard s != settings else { return }
        settings = s
        board.settings = s.view
        file.save(s)
        apply(force: true)
        updatePanel()
        changed()
    }

    /// Sends the settings when something they are built from changed (the
    /// devices' IDs, the UTC offset), or always with `force`.
    func apply(force: Bool = false) {
        let ids = Dictionary(model.devices.compactMap { d in d.deviceID.map { (d.key, $0) } }, uniquingKeysWith: { a, _ in a })
        let offset = TimeZone.current.secondsFromGMT() / 60
        guard force || ids != sentDevices || offset != sentOffset else { return }
        // Nothing to say to a core that was never told and is off anyway.
        if !settings.enabled, configured == nil, sentOffset == nil, !force { return }
        sentDevices = ids
        sentOffset = offset
        let ctx = SmartAlertSettings.CoreContext(
            deviceID: { ids[$0] }, name: { [model] in model.name(of: $0) }, utcOffsetMinutes: offset,
            secret: { [weak self] in self?.secret($0) }, allCameras: model.allCameras,
            entrance: { [model] in model.cameraOptions($0).entrance })
        file.saveStored(
            settings.storedJSON(
                name: { [model] in model.name(of: $0) }, allCameras: model.allCameras,
                entrance: { [model] in model.cameraOptions($0).entrance }))
        do throws(CoreStatus) {
            try bridge.smartAlertsAPI().alertsConfigure(json: settings.coreJSON(ctx))
            sendError = nil
        } catch .unsupported {
            sendError = "This core has no smart alerts."
        } catch {
            sendError = "The settings were not taken (\(error))."
        }
        changed()
    }

    // MARK: Secrets (the Keychain only)

    func secret(_ s: SmartAlertSecret) -> String? { credentials.password(for: s.account) }
    func hasSecret(_ s: SmartAlertSecret) -> Bool { credentials.hasPassword(for: s.account) }

    /// Saves a secret (an empty one deletes it) and sends the settings again.
    @discardableResult
    func setSecret(_ value: String, for s: SmartAlertSecret) -> Bool {
        let ok: Bool
        if value.isEmpty {
            credentials.deletePassword(for: s.account)
            ok = true
        } else {
            ok = credentials.setPassword(value, for: s.account)
        }
        apply(force: true)
        return ok
    }

    /// Removes a sensor and its Keychain items.
    func removeSensor(_ key: String) {
        update { $0.sensors.removeAll { $0.key == key } }
        credentials.deletePassword(for: SmartAlertSecret.mqttPassword(key).account)
    }

    // MARK: Events

    func handle(_ event: SmartAlertEvent) {
        let now = model.clock.now
        switch event {
        case .verifyRequest(let r):
            requests[r.request] = r
            if requests.count > 128, let oldest = requests.keys.sorted().first { requests[oldest] = nil }
            board.request(r, now: now)
        case .verifyRequestDone(let key, _):
            requests[key] = nil
        case .verifyStep(let trigger, let json):
            guard let step = JSONValue.parse(json)?["step"] else { break }
            board.step(triggerID: trigger, step, now: now)
            if step["step"]?.string == "delivery", let i = step["output"]?.int64 {
                lastDelivery[Int(i)] = (step["ok"]?.bool ?? false, Date())
            }
        case .alert(_, let json):
            if let v = JSONValue.parse(json) { board.alertEvent(v, now: now) }
        case .configured(let json):
            configured = JSONValue.parse(json)
        case .triggerRejected:
            break
        }
        for body in eventListeners.values { body(event) }
        updatePanel()
        changed()
    }

    /// A request by key, while it is open.
    func request(_ key: String) -> VerifyRequest? { requests[key] }

    /// The open requests of a check (by trigger ID), oldest first.
    func requests(trigger: UInt64) -> [VerifyRequest] {
        requests.values.filter { $0.triggerIDs.contains(trigger) }.sorted { $0.request < $1.request }
    }

    /// The detector's answer to a request (the boxes it sent the core), and
    /// the picture it checked: the live view draws them.
    func recordDetections(request key: String, detections: [AlertDetection], picture: CGImage? = nil) {
        guard let r = requests[key] else { return }
        let sensors = settings.sensors
        board.answered(
            r, detections: detections,
            region: { [model, settings] check in
                guard let ref = model.cameraRef(r.camera) else { return [] }
                if let i = check.sensor, sensors.indices.contains(i) {
                    return sensors[i].cameras.first { $0.camera == ref }?.roi ?? []
                }
                // A camera's own trigger: its first include zone, if any.
                return settings.camera(ref).zones.first { $0.include && $0.points.count >= 3 }?.points ?? []
            }, now: model.clock.now)
        if let picture {
            for id in r.triggerIDs { pictures[id, default: [:]][r.camera] = picture }
        }
        changed()
    }

    /// From the model's one-second timer (and the self-test after advancing
    /// its clock): checks whose time is up go.
    func tick() {
        guard let deadline = board.deadline, model.clock.now >= deadline else { return }
        let removed = board.tick(now: model.clock.now)
        guard !removed.isEmpty else { return }
        for id in removed { pictures[id] = nil }
        updatePanel()
        changed()
    }

    func dismiss(_ id: UInt64) {
        board.dismiss(id)
        pictures[id] = nil
        updatePanel()
        changed()
    }

    func dismissAll() {
        board.dismissAll()
        pictures = pictures.filter { board.check($0.key) != nil }
        updatePanel()
        changed()
    }

    func use(_ id: UInt64) {
        board.use(id, now: model.clock.now)
    }

    func hover(_ id: UInt64, _ inside: Bool) {
        board.hover(id, inside, now: model.clock.now)
        changed()
    }

    /// Where a check shows in a window: in its tile when the window shows the
    /// camera, else per the setting.
    func place(_ check: VerifyCheck, onScreen: Bool) -> VerifyPlace {
        VerifyPlace.place(settings.view.placement, onScreen: onScreen)
    }

    /// The checks for the verification window: those whose place is the
    /// window (the camera on no viewer window with the window setting, or
    /// "always a window").
    var panelChecks: [VerifyCheck] {
        board.shown.filter { c in
            let onScreen = c.primaryCamera.flatMap { model.cameraRef($0) }.map(isOnScreen) ?? false
            return place(c, onScreen: onScreen) == .window
        }
    }

    private func updatePanel() { showPanel(!panelChecks.isEmpty) }

    // MARK: Stills for drawing regions

    /// A still of a camera for drawing zones and regions: the latest
    /// picture of its live stream (a tile showing it, or one opened for at
    /// most 5 s), rendered to an image. Nil when no picture came.
    func still(of camera: CameraRef) async -> CGImage? {
        let leased = model.leases.tile(for: camera) == nil
        if leased { _ = model.leases.acquire(camera) }
        defer { if leased { model.leases.release(camera) } }
        for _ in 0..<50 {
            if let job = stillJob(camera) {
                guard let image = await model.pictureRenderer.render(job) else { return nil }
                return PictureEncoder.cgImage(image)
            }
            try? await Task.sleep(for: .milliseconds(100))
        }
        return nil
    }

    private func stillJob(_ camera: CameraRef) -> PictureRender? {
        guard let tile = model.leases.tile(for: camera), let picture = tile.latestPicture() else { return nil }
        let snapshot = tile.snapshot
        let mapping = PictureMapping(
            snapshot: snapshot, pictureSize: PixelSize(width: UInt32(picture.width), height: UInt32(picture.height)))
        let layout = CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height))
        guard layout.width >= 1, layout.height >= 1 else { return nil }
        let color =
            snapshot.color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
            ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
        let whole = PictureRegion.whole(
            layoutSize: layout, offset: CGPoint(x: CGFloat(mapping.offset.x), y: CGFloat(mapping.offset.y)))
        return PictureRender(picture: picture, color: color, source: whole.rect, width: whole.width, height: whole.height)
    }

    // MARK: The event log

    func log(_ filter: SmartAlertLogFilter, beforeMs: Int64? = nil) -> SmartAlertLogPage? {
        guard let text = try? bridge.smartAlertsAPI().alertsGet(query: filter.query(beforeMs: beforeMs)),
            let page = SmartAlertLogPage.parse(text)
        else { return nil }
        status = page.status
        return page
    }

    /// Reads the service's status (the settings screen's lines).
    func refreshStatus() {
        var f = SmartAlertLogFilter()
        f.limit = 1
        _ = log(f)
        changed()
    }

    @discardableResult
    func feedback(alertID: UInt64, useful: Bool, reason: AlertFeedbackReason?) -> Bool {
        do throws(CoreStatus) {
            try bridge.smartAlertsAPI().alertFeedback(
                json: AlertFeedbackReason.json(alertID: alertID, useful: useful, reason: reason))
            return true
        } catch {
            return false
        }
    }

    @discardableResult
    func mute(_ camera: AlertCamera, seconds: UInt32) -> Bool {
        do throws(CoreStatus) {
            try bridge.smartAlertsAPI().alertMute(cameraJSON: camera.json, seconds: seconds)
            refreshStatus()
            return true
        } catch {
            return false
        }
    }

    func snapshot(alertID: UInt64) -> NSImage? {
        guard let data = try? bridge.smartAlertsAPI().alertSnapshotGet(alertID: alertID) else { return nil }
        return NSImage(data: data)
    }

    // MARK: Status lines

    /// The settings screen's line for a sensor (its input's result and, when
    /// the core reports it, the connection's state).
    func sensorStatus(_ index: Int) -> String? {
        if let live = (status?["inputs"]?.array)?.first(where: { $0["index"]?.int64 == Int64(index) }) {
            if let state = live["state"]?.string {
                switch state {
                case "connected": return "Connected"
                case "login_rejected": return "Login rejected (not retried; save again to restart)"
                case let s: return s.replacingOccurrences(of: "_", with: " ").capitalized
                }
            }
        }
        guard let entry = (configured?["inputs"]?.array)?.first(where: { $0["index"]?.int64 == Int64(index) }) else {
            return settings.enabled ? nil : "Smart alerts are off"
        }
        if entry["ok"]?.bool == true { return "Started" }
        return "Not started: \(entry["error"]?.string ?? "unknown error")"
    }

    /// The Home Assistant output's line: its last delivery, else whether the
    /// core took its settings.
    var homeAssistantStatus: String? {
        let index = settings.homeAssistant.enabled ? 1 : nil
        if let index, let last = lastDelivery[index] {
            let f = DateFormatter()
            f.timeStyle = .medium
            return "Last alert \(last.ok ? "delivered" : "not delivered") at \(f.string(from: last.at))"
        }
        guard let index, let entry = (configured?["outputs"]?.array)?.first(where: { $0["index"]?.int64 == Int64(index) })
        else { return nil }
        if entry["ok"]?.bool == true { return "Ready" }
        return "Not started: \(entry["error"]?.string ?? "no output module for Home Assistant in this core")"
    }

    /// "Test": asks Home Assistant's API root with the token (a read only
    /// call; nothing is turned on). The answer goes into
    /// `homeAssistantTest`.
    func testHomeAssistant() {
        let base = settings.homeAssistant.baseURL.trimmingCharacters(in: .whitespaces)
        if let why = AlertEndpoint.checkHomeAssistantURL(base) {
            homeAssistantTest = why.prefix(1).uppercased() + why.dropFirst()
            return
        }
        guard let token = secret(.homeAssistantToken), !token.isEmpty else {
            homeAssistantTest = "No access token saved."
            return
        }
        guard let url = URL(string: base.hasSuffix("/") ? base + "api/" : base + "/api/") else {
            homeAssistantTest = "The address is not a URL."
            return
        }
        homeAssistantTest = "Testing…"
        var req = URLRequest(url: url, timeoutInterval: 5)
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let session = URLSession(configuration: .ephemeral, delegate: NoRedirects(), delegateQueue: nil)
        session.dataTask(with: req) { [weak self] _, response, error in
            let code = (response as? HTTPURLResponse)?.statusCode
            let text: String =
                switch code {
                case 200?: "Connected: the API answered."
                case 401?, 403?: "Token rejected (\(code ?? 0))."
                case let c?: "Home Assistant answered \(c)."
                case nil: "No answer: \(error.map { ($0 as NSError).localizedDescription } ?? "unknown error")."
                }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self?.homeAssistantTest = text
                    self?.changed()
                }
            }
            session.finishTasksAndInvalidate()
        }.resume()
        changed()
    }

    /// Redirects are never followed (the token must not go elsewhere).
    private final class NoRedirects: NSObject, URLSessionTaskDelegate {
        func urlSession(
            _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
            newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void
        ) { completionHandler(nil) }
    }

    // MARK: Observing

    func subscribe(_ body: @escaping () -> Void) -> UUID {
        let token = UUID()
        listeners[token] = body
        return token
    }

    func unsubscribe(_ token: UUID) { listeners[token] = nil }

    /// Every smart-alert event after the views took it (desktop
    /// notifications, the detector).
    func subscribeEvents(_ body: @escaping (SmartAlertEvent) -> Void) -> UUID {
        let token = UUID()
        eventListeners[token] = body
        return token
    }

    func unsubscribeEvents(_ token: UUID) { eventListeners[token] = nil }

    private func changed() {
        for body in listeners.values { body() }
    }
}
