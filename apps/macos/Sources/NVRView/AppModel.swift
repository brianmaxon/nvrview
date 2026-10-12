import AppKit
import FakeCore
import Foundation
import NVRCore
import Observation
import VideoKit

#if NVRVIEW_REAL_CORE
import NVRCoreC
#endif

/// What a window shows.
enum WindowContent: Equatable {
    /// A layout the core stores; edits are saved back to it.
    case layout(id: String)
    /// A group's cameras as an even grid; edits stay in the window until
    /// saved as a layout.
    case group(id: String)
    /// An unsaved arrangement (the first window before any layout exists).
    case scratch(name: String, root: LayoutNode)
}

/// The app's state, shared by every window: the core (through `CoreBridge`),
/// the configured devices and what the core says about them, the layouts
/// (stored by the core), the groups, the detections, and who is listening.
///
/// Main actor only. The core's events are forwarded to the main queue in
/// order. Views observe it through Observation (the SwiftUI sidebar) or
/// `subscribe` (the AppKit stages).
@MainActor
@Observable
final class AppModel {
    struct DeviceRuntime: Identifiable {
        var config: DeviceConfig
        /// The per-run device ID while the device is added to the core.
        var deviceID: UInt64?
        var link = DeviceLink()
        var reportedName: String?
        var model: String?
        var channels: [ChannelInfo] = []

        /// The name layouts and groups give the device's cameras: the core's
        /// stable device key once known (`nv_device_key`), else the app's.
        var key: String { config.coreKey ?? config.key }
        var id: String { config.key }
    }

    @ObservationIgnored let bridge: CoreBridge
    /// Set with `--fake`, for the self-test's contract checks.
    @ObservationIgnored let fake: FakeCore?
    @ObservationIgnored let leases: TileLeases
    /// Since when each tile has had a message a retry could help.
    @ObservationIgnored private var retryWatch = TileRetry.Watch()
    @ObservationIgnored private let credentials: any CredentialStore
    @ObservationIgnored private let deviceFile: JSONFileStore<DeviceConfig>
    @ObservationIgnored private let groupFile: JSONFileStore<CameraGroup>
    @ObservationIgnored private let cameraFile: JSONFileStore<CameraOptions>
    /// Doorbell calls in progress (docs/UX.md "Doorbell calls").
    @ObservationIgnored private(set) var calls: DoorbellCalls!
    @ObservationIgnored private let taps = TapRouter()
    /// Who wants to hear of every detection (the windows' insets).
    @ObservationIgnored private var detectionListeners: [UUID: (DetectionEvent, CameraRef, Date) -> Void] = [:]
    @ObservationIgnored private var player: AudioPlayer?
    @ObservationIgnored private var observers: [UUID: () -> Void] = [:]
    @ObservationIgnored private var expiry: Timer?
    @ObservationIgnored private var closed = false
    /// Devices and passwords given for this run only (`--host`, `--fake`).
    @ObservationIgnored private var transient: [String: String] = [:]
    /// Opens a window (set by the app delegate).
    @ObservationIgnored var openWindow: (WindowContent) -> Void = { _ in }
    /// Pop a call out into its own window, or back into the insets (set by
    /// the app delegate; docs/UX.md "A call in its own window").
    @ObservationIgnored var popOutCall: (CameraRef) -> Void = { _ in }
    @ObservationIgnored var popInCall: (CameraRef) -> Void = { _ in }
    /// Self-update (set by the app delegate); receives the core's `update`
    /// events.
    @ObservationIgnored var updater: Updater?

    private(set) var devices: [DeviceRuntime] = []
    /// The "find devices" list (docs/UX.md "Finding devices"). The core
    /// searches only while a screen that shows it asked for it.
    private(set) var discovery = DiscoveryList()
    /// Whether the add-device screen is on screen (it starts a search when
    /// it appears, and again when its window is shown after a close).
    @ObservationIgnored var discoveryPanelShown = false
    /// The add-device form (shared so a chosen found device fills it).
    @ObservationIgnored let deviceForm = DeviceForm()
    private(set) var layouts: [SavedLayout] = []
    private(set) var groups: [CameraGroup] = []
    /// Per-camera options (entrance, doorbell talk-back and ring), saved.
    private(set) var cameraSettings: [CameraOptions] = []
    /// Each fisheye camera's image circle, found from its first pictures.
    @ObservationIgnored private var fisheyeCircles: [CameraRef: FisheyeCircleTracker] = [:]
    private(set) var detections = DetectionTracker()
    /// What the sidebar rows show for the detections (held briefly after a
    /// stop, like the tiles' corner indicators).
    private(set) var indicators = IndicatorBoard()
    /// Raised alerts (docs/UX.md "Alerts"), shared by every window.
    private(set) var alerts = AlertCenter()
    private(set) var audible: Set<CameraRef> = []
    /// False when the core has no layouts store (an ABI 2 core): layouts
    /// then last only for this session.
    private(set) var layoutsStored = true
    /// The core's timing hooks, by `what` (nanoseconds).
    private(set) var timings: [String: UInt64] = [:]
    /// The last tier switch of each tile (by core tile ID) and when it
    /// happened; the frame-rate readout shows it briefly.
    private(set) var switchNotes: [UInt64: (timing: TierSwitchTiming, at: Double)] = [:]
    static let switchNoteSeconds = 6.0
    /// The viewer's clock: animations, auto-hide, and how long notes show
    /// (the system clock; a manual one in the self-test).
    @ObservationIgnored let clock: any ViewerClock
    /// A message for the user (shown in the sidebar footer), if any.
    var notice: String?
    /// Tile headers show the device's and the shown frame rate (View menu;
    /// remembered in the user's defaults, off by default).
    var showFrameRates = UserDefaults.standard.bool(forKey: AppModel.showFrameRatesKey) {
        didSet {
            UserDefaults.standard.set(showFrameRates, forKey: Self.showFrameRatesKey)
            notify()
        }
    }
    static let showFrameRatesKey = "showFrameRates"
    /// The tile corner that holds the detection indicators (View menu;
    /// remembered, top right by default).
    var indicatorCorner = ChromeCorner.load() {
        didSet {
            indicatorCorner.save()
            notify()
        }
    }
    /// The sidebar lists only channels that are connected (View menu;
    /// remembered). On by default: an NVR advertises every slot it has, and
    /// most of a large unit's can be empty. Unset in the defaults means on.
    var hideOfflineChannels = UserDefaults.standard.object(forKey: AppModel.hideOfflineKey) as? Bool ?? true {
        didSet {
            UserDefaults.standard.set(hideOfflineChannels, forKey: Self.hideOfflineKey)
            notify()
        }
    }
    static let hideOfflineKey = "hideOfflineChannels"
    /// Doorbell call transcripts go into the event log and, one text file
    /// per call, into the Transcripts folder only when this is on (recording
    /// a conversation may need consent). Off by default. Kept with the call
    /// settings (`callDefaults`).
    var saveCallTranscripts = UserDefaults.standard.bool(forKey: AppModel.saveTranscriptsKey) {
        didSet {
            callDefaults.set(saveCallTranscripts, forKey: Self.saveTranscriptsKey)
            notify()
        }
    }
    static let saveTranscriptsKey = "saveCallTranscripts"
    /// Where saved transcripts go (`saveCallTranscripts`).
    @ObservationIgnored let transcripts = TranscriptStore(folder: JSONFileStore<DeviceConfig>.appSupport("Transcripts"))
    /// The stage corner for alert insets; nil: opposite the indicators.
    var insetCornerChoice: ChromeCorner? = UserDefaults.standard.string(forKey: AppModel.insetCornerKey)
        .flatMap(ChromeCorner.init(rawValue:))
    {
        didSet {
            UserDefaults.standard.set(insetCornerChoice?.rawValue, forKey: Self.insetCornerKey)
            notify()
        }
    }
    static let insetCornerKey = "insetCorner"
    /// Where the call settings below are kept: the standard defaults, a
    /// throwaway suite in the self-test (`useCallDefaults`).
    @ObservationIgnored private(set) var callDefaults = UserDefaults.standard
    /// "Open calls in their own window" (docs/UX.md "A call in its own
    /// window"): a ring or Start Conversation opens the call in a call
    /// window unless the doorbell's own choice says otherwise. Off by default.
    var callsOpenInWindow = UserDefaults.standard.bool(forKey: AppModel.callsOpenInWindowKey) {
        didSet {
            callDefaults.set(callsOpenInWindow, forKey: Self.callsOpenInWindowKey)
            notify()
        }
    }
    static let callsOpenInWindowKey = "callsOpenInWindow"
    /// "Open call windows": where I left it (the default) or the top right
    /// of the active screen.
    var callWindowPlacement = AppModel.placement(in: .standard) {
        didSet {
            callDefaults.set(callWindowPlacement.rawValue, forKey: Self.callWindowPlacementKey)
            notify()
        }
    }
    static let callWindowPlacementKey = "callWindowPlacement"

    private static func placement(in defaults: UserDefaults) -> CallWindowPlacement.Mode {
        defaults.string(forKey: callWindowPlacementKey).flatMap(CallWindowPlacement.Mode.init(rawValue:)) ?? .remembered
    }

    /// Keeps the call settings in `defaults` from now on (read from there).
    func useCallDefaults(_ defaults: UserDefaults) {
        callDefaults = defaults
        callsOpenInWindow = defaults.bool(forKey: Self.callsOpenInWindowKey)
        callWindowPlacement = Self.placement(in: defaults)
        saveCallTranscripts = defaults.bool(forKey: Self.saveTranscriptsKey)
    }
    /// Where the insets go now.
    var insetCorner: ChromeCorner { insetCornerChoice ?? indicatorCorner.opposite }

    /// The channels the sidebar lists for a device.
    func listedChannels(_ device: DeviceRuntime) -> [ChannelInfo] {
        ChannelFilter.visible(device.channels, hideOffline: hideOfflineChannels)
    }

    /// Gives the core's event thread a way back to the model on the main queue.
    private final class Target: @unchecked Sendable {
        // Touched only on the main queue.
        weak var model: AppModel?
    }

    init(
        factory: CoreFactory, fakeBox: FakeBox?, credentials: any CredentialStore,
        deviceFile: JSONFileStore<DeviceConfig>, groupFile: JSONFileStore<CameraGroup>,
        cameraFile: JSONFileStore<CameraOptions> = JSONFileStore(file: nil),
        clock: any ViewerClock = SystemViewerClock(), callServices: CallServices? = nil
    ) throws {
        self.clock = clock
        let target = Target()
        let taps = taps
        bridge = try CoreBridge(
            factory: factory,
            onEvent: { event in
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { target.model?.handle(event) }
                }
            },
            onAudioTap: { block, tile in taps.deliver(block, tile: tile) })
        fake = fakeBox?.core
        leases = TileLeases(bridge: bridge)
        self.credentials = credentials
        self.deviceFile = deviceFile
        self.groupFile = groupFile
        self.cameraFile = cameraFile
        target.model = self
        calls = DoorbellCalls(model: self, services: callServices ?? .system(player: { [weak self] in self?.player }), taps: taps)
        leases.hasMiddleTier = { [weak self] camera in
            guard let info = self?.channelInfo(camera) else { return false }
            return info.tiers.contains("extern") && !info.isMultiLens
        }
        leases.resolve = { [weak self] key in self?.devices.first { $0.key == key }?.deviceID }
        leases.onTileChange = { [weak self] camera, tile in
            guard let self else { return }
            if let tile { try? bridge.setTileAudio(tile, audible: audible.contains(camera)) }
            calls.tileChanged(camera)
        }
    }

    /// The real core, if this build has one.
    static func realFactory(dataDirectory: URL? = nil) -> CoreFactory? {
        #if NVRVIEW_REAL_CORE
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Logs/NVRView", isDirectory: true)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        // The core keeps layouts and device keys here.
        let data = dataDirectory?.appendingPathComponent("core") ?? JSONFileStore<DeviceConfig>.appSupport("core")
        try? FileManager.default.createDirectory(at: data, withIntermediateDirectories: true)
        return CCore.factory(logDirectory: logs.path, dataDirectory: data.path)
        #else
        return nil
        #endif
    }

    // MARK: Start and stop

    /// Loads the stores, adds every device to the core, and starts audio.
    func start(extraDevices: [(DeviceConfig, String)] = [], muted: Bool = false) {
        groups = groupFile.load()
        cameraSettings = cameraFile.load()
        var configs = deviceFile.load()
        for (config, password) in extraDevices where !configs.contains(where: { $0.key == config.key }) {
            // For this run only: never written to the Keychain or the file.
            configs.append(config)
            transient[config.key] = password
        }
        devices = configs.map { DeviceRuntime(config: $0) }
        reloadLayouts()
        for d in devices { connect(d.key) }
        startAudio(muted: muted)
        expiry = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.expireDetections()
                self?.tickCalls()
            }
        }
        notify()
    }

    private func startAudio(muted: Bool) {
        guard player == nil, !closed else { return }
        do {
            let p = try bridge.makeAudioPlayer()
            if muted { p.outputVolume = 0 }
            try p.start()
            player = p
        } catch {
            notice = "Audio output unavailable"
        }
    }

    /// Closes the core: every session, every tile. Call on every exit path.
    func shutdown() {
        guard !closed else { return }
        closed = true
        expiry?.invalidate()
        calls.endAll()
        player?.stop()
        player = nil
        leases.closeAll()
        bridge.shutdown()
        for i in devices.indices { devices[i].deviceID = nil }
    }

    // MARK: Observing

    /// Calls `body` after every change (on the main actor). Keep the token;
    /// pass it to `unsubscribe`.
    func subscribe(_ body: @escaping () -> Void) -> UUID {
        let token = UUID()
        observers[token] = body
        return token
    }

    func unsubscribe(_ token: UUID) { observers[token] = nil }

    private func notify() {
        for body in observers.values { body() }
    }

    /// Something the views show changed (doorbell calls).
    func changed() { notify() }

    /// Calls `body` for every detection, motion and alarm event, with its
    /// camera and when it happened (the windows' insets). Keep the token.
    func subscribeDetections(_ body: @escaping (DetectionEvent, CameraRef, Date) -> Void) -> UUID {
        let token = UUID()
        detectionListeners[token] = body
        return token
    }

    func unsubscribeDetections(_ token: UUID) { detectionListeners[token] = nil }

    /// Steps the doorbell calls on the viewer clock (the one-second timer;
    /// the self-test after advancing its clock).
    func tickCalls() { calls.tick(now: clock.now) }

    /// A line for the session's event log. Only its first line goes to the
    /// system log: a saved transcript stays in the app's own log.
    func logEvent(_ text: String, transcriptFollows: Bool = false) {
        alerts.record(text, at: Date())
        let first = text.split(separator: "\n", maxSplits: 1).first.map(String.init) ?? text
        NSLog("%@", first + (transcriptFollows ? " (transcript saved in the event log)" : ""))
    }

    // MARK: Camera options

    /// A camera's options (defaults when never set).
    func cameraOptions(_ camera: CameraRef) -> CameraOptions {
        cameraSettings.first { $0.camera == camera } ?? CameraOptions(camera: camera)
    }

    private func updateOptions(_ camera: CameraRef, _ change: (inout CameraOptions) -> Void) {
        var o = cameraOptions(camera)
        change(&o)
        cameraSettings.removeAll { $0.camera == camera }
        if !o.isDefault { cameraSettings.append(o) }
        cameraFile.save(cameraSettings)
        notify()
    }

    /// Marks a camera as an entrance: a person on it opens an alert inset.
    func setEntrance(_ camera: CameraRef, _ on: Bool) { updateOptions(camera) { $0.entrance = on } }

    /// How a fisheye camera is mounted (its panorama and virtual views follow).
    func setFisheyeMount(_ camera: CameraRef, _ mount: FisheyeMount) {
        updateOptions(camera.firstLens) { $0.fisheyeMount = mount }
    }

    /// Whether the camera is a fisheye camera the app can dewarp.
    func isFisheye(_ camera: CameraRef) -> Bool { camera.lens == 0 && channelInfo(camera)?.fisheye == true }

    /// The lens of a fisheye camera for its newest picture: the image circle
    /// found in its first pictures (FisheyeCircleTracker; a few tries on the
    /// CPU, then never again), kept per camera for the run.
    func fisheyeLens(_ camera: CameraRef, picture: DecodedPicture?) -> FisheyeLens? {
        let camera = camera.firstLens
        var tracker = fisheyeCircles[camera] ?? FisheyeCircleTracker()
        if let picture, tracker.wantsTry(now: clock.now) {
            let pb = picture.pixelBuffer
            let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
            CVPixelBufferLockBaseAddress(pb, .readOnly)
            var found: FisheyeCircle?
            if let base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) {
                let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
                let bytes = base.assumingMemoryBound(to: UInt8.self)
                found = FisheyeCircleDetector.detect(width: w, height: h) { x, y in bytes[y * stride + x] }
            }
            CVPixelBufferUnlockBaseAddress(pb, .readOnly)
            tracker.record(found, now: clock.now, width: w, height: h)
            fisheyeCircles[camera] = tracker
            if let c = tracker.circle {
                NSLog(
                    "fisheye circle for channel %d: center %.3f, %.3f radius %.3f%@", camera.channel + 1, c.cx, c.cy,
                    c.r, tracker.fellBack ? " (not found; inscribed circle)" : "")
            }
        }
        guard let circle = tracker.circle else { return nil }
        let aspect = picture.map {
            Double(CVPixelBufferGetWidth($0.pixelBuffer)) / Double(max(CVPixelBufferGetHeight($0.pixelBuffer), 1))
        } ?? 1
        return FisheyeLens(circle: circle, aspect: aspect)
    }

    /// Where a camera's calls open, overriding "Open calls in their own
    /// window" (nil: that setting decides).
    func setCallPresentation(_ camera: CameraRef, _ presentation: CallPresentation?) {
        updateOptions(camera.firstLens) { $0.callWindow = presentation.map { $0 == .window } }
    }

    /// Where a call on `camera` opens now (a ring or Start Conversation).
    func callPresentation(for camera: CameraRef) -> CallPresentation {
        CallPresentation.resolve(global: callsOpenInWindow, override: cameraOptions(camera.firstLens).callPresentation)
    }

    /// A doorbell plays when it rings even if nothing was playing.
    func setUnmuteOnRing(_ camera: CameraRef, _ on: Bool) { updateOptions(camera.firstLens) { $0.unmuteOnRing = on } }

    /// Talk-back for a doorbell (camera motion control; off by default):
    /// passed to the core at once, and after every connection.
    func setAllowTalk(_ camera: CameraRef, _ on: Bool) {
        let camera = camera.firstLens
        updateOptions(camera) { $0.allowTalk = on }
        logEvent("Talk-back \(on ? "allowed" : "turned off"): channel \(camera.channel + 1)")
        if let id = runtime(camera.device)?.deviceID, (0...255).contains(camera.channel) {
            try? bridge.setTalkPermission(device: id, channel: UInt8(camera.channel), allowed: on)
        }
    }

    /// Passes the user's talk-back permissions for a device's doorbells to
    /// the core (it keeps them for this run only).
    private func applyTalkPermissions(_ key: String) {
        guard let id = runtime(key)?.deviceID else { return }
        for o in cameraSettings where o.camera.device == key && o.allowTalk && (0...255).contains(o.camera.channel) {
            try? bridge.setTalkPermission(device: id, channel: UInt8(o.camera.channel), allowed: true)
        }
    }

    // MARK: Devices

    /// By camera key or configuration key.
    func runtime(_ key: String) -> DeviceRuntime? { index(key).map { devices[$0] } }

    private func key(forDevice id: UInt64) -> String? { devices.first { $0.deviceID == id }?.key }

    private func index(_ key: String) -> Int? { devices.firstIndex { $0.key == key || $0.config.key == key } }

    /// Whether a password is known, without reading the Keychain's secret
    /// (Settings shows "Saved in the Keychain" with no permission prompt).
    func hasPassword(_ key: String) -> Bool {
        guard let configKey = runtime(key)?.config.key else { return false }
        return transient[configKey] != nil || credentials.hasPassword(for: configKey)
    }

    /// By configuration key (the Keychain account).
    private func password(_ configKey: String) -> String? {
        transient[configKey] ?? credentials.password(for: configKey)
    }

    /// Adds the device to the core with its stored password.
    private func connect(_ key: String) {
        guard let i = index(key), !closed else { return }
        let config = devices[i].config
        guard let password = password(config.key) else {
            devices[i].link = DeviceLink(state: .other("needs password"), since: Self.now)
            return
        }
        do throws(CoreStatus) {
            let id = try bridge.addDevice(host: config.host, port: config.port, user: config.user, password: password)
            devices[i].deviceID = id
            if devices[i].link.state == nil { devices[i].link = DeviceLink(state: .connecting, since: Self.now) }
            if let coreKey = bridge.deviceKey(id), coreKey != devices[i].key {
                adoptCoreKey(coreKey, at: i)
            }
            applyTalkPermissions(devices[i].key)
            leases.reopen(device: devices[i].key)
        } catch {
            devices[i].link = DeviceLink(state: .other("cannot add (status \(error.code))"), since: Self.now)
        }
    }

    /// The core names this device differently from what layouts and groups
    /// used so far (first run with a core that assigns keys, or the address
    /// changed): rename its cameras everywhere the app can.
    private func adoptCoreKey(_ coreKey: String, at i: Int) {
        let old = devices[i].key
        leases.closeTiles(device: old)
        devices[i].config.coreKey = coreKey
        if transient[devices[i].config.key] == nil {
            deviceFile.save(devices.filter { transient[$0.config.key] == nil }.map(\.config))
        }
        for g in groups.indices {
            groups[g].cameras = groups[g].cameras.map { $0.device == old ? $0.rekeyed(to: coreKey) : $0 }
        }
        groupFile.save(groups)
        cameraSettings = cameraSettings.map { o in
            var o = o
            if o.camera.device == old { o.camera = o.camera.rekeyed(to: coreKey) }
            return o
        }
        cameraFile.save(cameraSettings)
        audible = Set(audible.map { $0.device == old ? $0.rekeyed(to: coreKey) : $0 })
        for layout in layouts where layout.root.cameras.contains(where: { $0.device == old }) {
            var l = layout
            l.root = layout.root.rekeying(device: old, to: coreKey)
            saveLayout(l)
        }
    }

    /// Removes the device from the core (its tiles end; leases stay).
    private func disconnect(_ key: String) {
        guard let i = index(key) else { return }
        let key = devices[i].key
        leases.closeTiles(device: key)
        if let id = devices[i].deviceID { try? bridge.removeDevice(id) }
        devices[i].deviceID = nil
        devices[i].link = DeviceLink()
        detections.clear(device: key)
        refreshIndicators()
    }

    /// Saves a new or edited device and (re)connects it. `password` nil
    /// keeps the stored one.
    func saveDevice(_ config: DeviceConfig, password: String?) {
        if let password, !password.isEmpty {
            if transient[config.key] != nil {
                transient[config.key] = password
            } else if !credentials.setPassword(password, for: config.key) {
                notice = "Could not save the password in the Keychain"
            }
        }
        if let i = index(config.key) {
            disconnect(config.key)
            // Kept until the core says otherwise (a new address gets a new
            // key, adopted on connect).
            var config = config
            config.coreKey = config.coreKey ?? devices[i].config.coreKey
            devices[i].config = config
        } else {
            devices.append(DeviceRuntime(config: config))
        }
        if transient[config.key] == nil {
            deviceFile.save(devices.filter { transient[$0.config.key] == nil }.map(\.config))
        }
        connect(config.key)
        notify()
    }

    /// Removes a device, its password, and its cameras from every group.
    func removeDevice(_ key: String) {
        guard let d = runtime(key) else { return }
        // Its calls end (and their windows close) before it goes.
        calls.endCalls(device: d.key)
        disconnect(key)
        devices.removeAll { $0.config.key == d.config.key }
        if transient.removeValue(forKey: d.config.key) == nil { credentials.deletePassword(for: d.config.key) }
        deviceFile.save(devices.filter { transient[$0.config.key] == nil }.map(\.config))
        let key = d.key
        for i in groups.indices { groups[i].cameras.removeAll { $0.device == key } }
        groupFile.save(groups)
        audible = audible.filter { $0.device != key }
        notify()
    }

    /// Adds the device again with its current password (after a failed
    /// login was fixed, or to retry).
    func reconnect(_ key: String) {
        disconnect(key)
        connect(key)
        notify()
    }

    // MARK: Cameras

    func channelInfo(_ camera: CameraRef) -> ChannelInfo? {
        runtime(camera.device)?.channels.first { $0.channel == camera.channel }
    }

    /// The channel's name; for a multi-lens camera with the lens ("Lens 2").
    func name(of camera: CameraRef) -> String {
        let info = channelInfo(camera)
        let base = info?.name.flatMap { $0.isEmpty ? nil : $0 } ?? "Channel \(camera.channel + 1)"
        guard info?.isMultiLens == true || camera.lens > 0 else { return base }
        return "\(base) · \(Self.lensName(camera.lens))"
    }

    /// The channel's name without the lens.
    func channelName(_ camera: CameraRef) -> String {
        channelInfo(camera)?.name.flatMap { $0.isEmpty ? nil : $0 } ?? "Channel \(camera.channel + 1)"
    }

    /// Generic lens words: "Lens 1", "Lens 2".
    static func lensName(_ lens: Int) -> String { "Lens \(lens + 1)" }

    /// The cameras of a channel: one per lens.
    func cameras(device key: String, channel c: ChannelInfo) -> [CameraRef] {
        (0..<min(c.lenses, CameraRef.maxLens + 1)).map { CameraRef(device: key, channel: c.channel, lens: $0) }
    }

    /// Whether `camera` is one lens of a multi-lens camera.
    func isMultiLens(_ camera: CameraRef) -> Bool { channelInfo(camera)?.isMultiLens == true }

    func deviceName(_ key: String) -> String { runtime(key)?.config.name ?? "Removed device" }

    /// Whether the camera detects objects itself (otherwise detection would
    /// come from this Mac).
    func hasCameraAI(_ camera: CameraRef) -> Bool { !(channelInfo(camera)?.detects.isEmpty ?? true) }

    /// Every camera of every device, in order.
    var allCameras: [CameraRef] {
        devices.flatMap { d in d.channels.flatMap { cameras(device: d.key, channel: $0) } }
    }

    /// The cameras "All Cameras" shows: every online channel of every
    /// connected device, in sidebar order, up to the layout limit.
    var onlineCameras: [CameraRef] { AllCameras.select(devices.map(Self.allCamerasDevice)) }

    /// The online cameras of one device (the device menu's own window).
    func onlineCameras(of device: DeviceRuntime) -> [CameraRef] {
        Array(AllCameras.cameras(of: Self.allCamerasDevice(device)).prefix(LayoutNode.maxLeaves))
    }

    private static func allCamerasDevice(_ d: DeviceRuntime) -> AllCamerasDevice {
        AllCamerasDevice(key: d.key, connected: d.link.state == .connected, channels: d.channels)
    }

    /// What a tile showing `camera` says instead of a picture, if anything.
    func veil(for camera: CameraRef) -> TileVeil? {
        guard let d = runtime(camera.device) else { return TileVeil(title: "Device removed") }
        let tile = leases.tile(for: camera)
        if d.link.state == .connected, !d.channels.isEmpty, channelInfo(camera)?.connected == false {
            return TileVeil(title: "Camera offline")
        }
        return TileVeil.make(
            link: d.link, status: tile?.snapshot.status, hasPicture: tile?.latestPicture() != nil, now: Self.now)
    }

    /// Whether the tile showing `camera` offers a Retry button: it has been
    /// without a picture, with a message a retry can help, for
    /// `TileRetry.offerAfter` (on the viewer clock, so a test sets the time).
    func retryOffered(for camera: CameraRef) -> Bool {
        guard let d = runtime(camera.device) else { return false }
        if d.link.state == .connected, !d.channels.isEmpty, channelInfo(camera)?.connected == false {
            _ = retryWatch.offered(camera, applies: false, now: clock.now)
            return false
        }
        let tile = leases.tile(for: camera)
        let applies = TileRetry.applies(
            link: d.link, status: tile?.snapshot.status, hasPicture: tile?.latestPicture() != nil)
        return retryWatch.offered(camera, applies: applies, now: clock.now)
    }

    /// The Retry button: the core restarts the camera's stream now (or
    /// connects now, when its device is waiting to), within its own floor.
    func retry(_ camera: CameraRef) {
        retryWatch.restart(camera, now: clock.now)
        guard let tile = leases.tile(for: camera) else { return }
        do throws(CoreStatus) {
            try bridge.retryTile(tile)
        } catch {
            NSLog("retry for a tile was not accepted: %@", String(describing: error))
        }
    }

    // MARK: Layouts

    func layout(_ id: String) -> SavedLayout? { layouts.first { $0.id == id } }

    func group(_ id: String) -> CameraGroup? { groups.first { $0.id == id } }

    private func reloadLayouts() {
        do {
            layouts = SavedLayout.list(fromJSON: try bridge.layoutsJSON())
            layoutsStored = true
        } catch {
            layoutsStored = false
        }
    }

    /// Stores a layout (insert or replace). Without a core store it is kept
    /// for this session only.
    @discardableResult
    func saveLayout(_ layout: SavedLayout) -> Bool {
        if layoutsStored {
            do throws(CoreStatus) {
                try bridge.putLayout(json: layout.json)
                reloadLayouts()
                notify()
                return true
            } catch {
                if error != .invalidArgument || (try? bridge.layoutsJSON()) != nil {
                    notice = error == .limit ? "Too many layouts; delete some first" : "The layout could not be saved"
                    notify()
                    return false
                }
                layoutsStored = false
            }
        }
        if let i = layouts.firstIndex(where: { $0.id == layout.id }) {
            var l = layout
            l.version = layouts[i].version + 1
            layouts[i] = l
        } else {
            var l = layout
            l.version = 1
            layouts.append(l)
        }
        notify()
        return true
    }

    func deleteLayout(_ id: String) {
        if layoutsStored {
            try? bridge.deleteLayout(id: id)
            reloadLayouts()
        } else {
            layouts.removeAll { $0.id == id }
        }
        notify()
    }

    func renameLayout(_ id: String, to name: String) {
        guard var l = layout(id), !name.isEmpty else { return }
        l.name = name
        saveLayout(l)
    }

    /// A new layout from a tree; returns its id.
    @discardableResult
    func createLayout(name: String, root: LayoutNode) -> String? {
        let l = SavedLayout(name: name, root: root)
        return saveLayout(l) ? l.id : nil
    }

    func uniqueLayoutName(_ base: String) -> String {
        let names = Set(layouts.map(\.name))
        if !names.contains(base) { return base }
        var n = 2
        while names.contains("\(base) \(n)") { n += 1 }
        return "\(base) \(n)"
    }

    // MARK: Groups

    @discardableResult
    func createGroup(name: String, cameras: [CameraRef]) -> String {
        let g = CameraGroup(id: CameraGroup.newID(), name: name, cameras: cameras)
        groups.append(g)
        groupFile.save(groups)
        notify()
        return g.id
    }

    func renameGroup(_ id: String, to name: String) {
        guard let i = groups.firstIndex(where: { $0.id == id }), !name.isEmpty else { return }
        groups[i].name = name
        groupFile.save(groups)
        notify()
    }

    func deleteGroup(_ id: String) {
        groups.removeAll { $0.id == id }
        groupFile.save(groups)
        notify()
    }

    func addCamera(_ camera: CameraRef, toGroup id: String) {
        guard let i = groups.firstIndex(where: { $0.id == id }), !groups[i].cameras.contains(camera) else { return }
        groups[i].cameras.append(camera)
        groupFile.save(groups)
        notify()
    }

    func removeCamera(_ camera: CameraRef, fromGroup id: String) {
        guard let i = groups.firstIndex(where: { $0.id == id }) else { return }
        groups[i].cameras.removeAll { $0 == camera }
        groupFile.save(groups)
        notify()
    }

    // MARK: Listening

    /// The speaker button: makes `camera` the audible one (or mutes it);
    /// with `additive`, adds or removes it from the audible set instead.
    func toggleListening(_ camera: CameraRef, additive: Bool) {
        setAudible(ListenPolicy.toggle(camera, in: audible, additive: additive))
    }

    func stopListening() { setAudible([]) }

    /// Makes exactly `new` audible.
    func setAudible(_ new: Set<CameraRef>) {
        let changed = audible.symmetricDifference(new)
        audible = new
        calls?.audibleChanged(new)
        for camera in changed {
            // Unmuting only routes audio already flowing in the core.
            if let tile = leases.tile(for: camera) {
                try? bridge.setTileAudio(tile, audible: new.contains(camera))
            }
        }
        notify()
    }

    /// The level meter's value: the camera's decoded audio, audible or not.
    func level(_ camera: CameraRef) -> AudioLevel? {
        guard let tile = leases.tile(for: camera) else { return nil }
        return bridge.audioLevel(tile)
    }

    // MARK: Events

    static var now: TimeInterval { ProcessInfo.processInfo.systemUptime }

    private func handle(_ event: CoreEvent) {
        guard !closed else { return }
        switch event {
        case .deviceState(let id, let state):
            guard let key = key(forDevice: id), let i = index(key) else { return }
            devices[i].link = DeviceLink(state: state, since: Self.now)
            switch state {
            case .connected: break
            case .closed:
                detections.clear(device: key)
                alerts.deviceLost(key)
                refreshIndicators()
            case .loginFailed:
                // Terminal: the core does not retry. The tiles end; new
                // credentials in Settings add the device again.
                detections.clear(device: key)
                alerts.deviceLost(key)
                refreshIndicators()
                leases.closeTiles(device: key)
            default:
                detections.clear(device: key)
                alerts.deviceLost(key)
                refreshIndicators()
            }
        case .deviceInfo(let id, let name, let model):
            guard let key = key(forDevice: id), let i = index(key) else { return }
            devices[i].reportedName = name
            devices[i].model = model
        case .channels(let id, let channels):
            guard let key = key(forDevice: id), let i = index(key) else { return }
            // Names may come in a later list than the slots: keep known ones.
            let old = devices[i].channels
            devices[i].channels = channels.map { c in
                var c = c
                if c.name == nil { c.name = old.first { $0.channel == c.channel }?.name }
                return c
            }
        case .tileClosed(let tile, _):
            leases.tileEnded(tile)
        case .tileError(let tile, let reason, _):
            leases.startFailed(tile: tile, reason: reason)
        case .detection(let d):
            guard let key = key(forDevice: d.device) else { return }
            let camera = d.camera(deviceKey: key)
            detections.apply(d, camera: camera)
            refreshIndicators()
            raiseAlerts(d, deviceKey: key)
            let at = Self.eventDate(d)
            if d.alarm, d.kind == "visitor", d.active { calls.ring(camera, at: at) }
            for body in detectionListeners.values { body(d, camera, at) }
        case .layoutsChanged:
            reloadLayouts()
        case .timing(let what, let tile, let ns, let detail):
            if timings[what] == nil { timings[what] = ns }
            if let tile, let detail {
                switchNotes[tile] = (detail, clock.now)
            }
        case .tileTier(let tile, let tier, .failed, _, let reason):
            leases.tierFailed(tile: tile, tier: tier, reason: reason)
        case .talk(let t):
            calls.talkEvent(t)
        case .discoveryStarted, .discovered, .discoveryDone:
            discovery.apply(event)
        case .update(let u):
            updater?.handle(u)
        case .tileTier, .audioStats, .tileStats:
            break
        case .recordings, .playback:
            // No recordings or playback views yet (milestone P, M1 and M2).
            break
        }
        notify()
    }

    // MARK: Finding devices

    /// Starts a search of the local network (the core never searches on its
    /// own); a search already running restarts. The list empties.
    func startDiscovery() {
        guard !closed else { return }
        do throws(CoreStatus) {
            try bridge.discoverStart()
            discovery.requested()
        } catch {
            discovery.failedToStart()
        }
        notify()
    }

    /// Stops the search (the screen closed).
    func stopDiscovery() {
        guard !closed else { return }
        bridge.discoverStop()
        discovery.stopped()
        notify()
    }

    /// A found device was chosen in the list: its address and port go into
    /// the add form.
    func chooseDiscovered(_ found: DiscoveredDevice) {
        guard found.isNative, !isAdded(found) else { return }
        deviceForm.fill(from: found)
        notify()
    }

    /// Whether a found device is one the user already added.
    func isAdded(_ found: DiscoveredDevice) -> Bool {
        found.isAdded(among: devices.map { (host: $0.config.host, port: $0.config.port) })
    }

    /// The tier switch note for a tile while it is recent.
    func switchNote(forTile tile: UInt64) -> String? {
        guard let n = switchNotes[tile], clock.now - n.at < Self.switchNoteSeconds else {
            return nil
        }
        return n.timing.note
    }

    private func expireDetections() {
        let now = clock.now
        let stale = switchNotes.filter { now - $0.value.at >= Self.switchNoteSeconds }
        if !stale.isEmpty {
            for key in stale.keys { switchNotes[key] = nil }
            notify()
        }
        let before = detections
        // A stop lost to a reconnect must not leave a badge up for good.
        detections.expire(now: HostClock.nowNanos(), maxAgeNanos: 120_000_000_000)
        if detections != before { notify() }
        refreshIndicators()
    }

    /// When an event happened: a sound alarm's start arrives seconds after
    /// the sound began, so it is dated by when the first matched beep
    /// arrived, on the core's clock.
    static func eventDate(_ d: DetectionEvent) -> Date {
        var at = Date()
        if let ns = d.atNanos, ns <= HostClock.nowNanos() {
            at = at.addingTimeInterval(-Double(HostClock.nowNanos() &- ns) / 1e9)
        }
        return at
    }

    private func raiseAlerts(_ d: DetectionEvent, deviceKey: String) {
        let at = Self.eventDate(d)
        let before = alerts.log.count
        guard alerts.apply(d, deviceKey: deviceKey, at: at) else { return }
        for entry in alerts.log.dropFirst(before) { NSLog("%@", entry.text) }
        notify()
    }

    /// The user dismissed a camera's alert: gone everywhere.
    func dismissAlert(_ camera: CameraRef) {
        let before = alerts.log.count
        guard alerts.dismiss(camera, at: Date()) != nil else { return }
        for entry in alerts.log.dropFirst(before) { NSLog("%@", entry.text) }
        notify()
    }

    /// Re-reads the detections for the sidebar's symbols (on events and the
    /// one-second tick: no per-frame work).
    private func refreshIndicators() {
        var board = indicators
        if board.update(detections, now: ProcessInfo.processInfo.systemUptime) { indicators = board }
    }
}

/// Hands the `FakeCore` a factory creates back to its caller.
final class FakeBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: FakeCore?
    var core: FakeCore? {
        get { lock.lock(); defer { lock.unlock() }; return stored }
        set { lock.lock(); stored = newValue; lock.unlock() }
    }
}

/// What the user set for one camera (saved in the app's data directory). The
/// doorbell options belong to a channel (its first lens).
struct CameraOptions: Codable, Equatable, Sendable {
    var camera: CameraRef
    /// A person on this camera opens an alert inset (docs/UX.md "Alert insets").
    var entrance = false
    /// A doorbell: audible when it rings even if nothing was playing.
    var unmuteOnRing = false
    /// A doorbell: talk-back allowed (camera motion control; off by default).
    var allowTalk = false
    /// A fisheye camera: how it is mounted (docs/UX.md "Fisheye cameras").
    var fisheyeMount = FisheyeMount.ceiling
    /// Where its calls open: true in their own window, false in the insets,
    /// nil as "Open calls in their own window" says (the default).
    var callWindow: Bool?

    /// The override for `CallPresentation.resolve`.
    var callPresentation: CallPresentation? { callWindow.map { $0 ? .window : .inset } }

    init(
        camera: CameraRef, entrance: Bool = false, unmuteOnRing: Bool = false, allowTalk: Bool = false,
        fisheyeMount: FisheyeMount = .ceiling
    ) {
        self.camera = camera
        self.entrance = entrance
        self.unmuteOnRing = unmuteOnRing
        self.allowTalk = allowTalk
        self.fisheyeMount = fisheyeMount
    }

    private enum CodingKeys: String, CodingKey { case camera, entrance, unmuteOnRing, allowTalk, fisheyeMount, callWindow }

    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(CameraRef.self, forKey: .camera)
        entrance = try c.decodeIfPresent(Bool.self, forKey: .entrance) ?? false
        unmuteOnRing = try c.decodeIfPresent(Bool.self, forKey: .unmuteOnRing) ?? false
        allowTalk = try c.decodeIfPresent(Bool.self, forKey: .allowTalk) ?? false
        fisheyeMount = (try? c.decodeIfPresent(FisheyeMount.self, forKey: .fisheyeMount)) ?? .ceiling
        callWindow = try c.decodeIfPresent(Bool.self, forKey: .callWindow)
    }

    var isDefault: Bool { !entrance && !unmuteOnRing && !allowTalk && fisheyeMount == .ceiling && callWindow == nil }
}
