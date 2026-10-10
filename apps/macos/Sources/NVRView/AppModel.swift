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
    @ObservationIgnored private let credentials: any CredentialStore
    @ObservationIgnored private let deviceFile: JSONFileStore<DeviceConfig>
    @ObservationIgnored private let groupFile: JSONFileStore<CameraGroup>
    @ObservationIgnored private var player: AudioPlayer?
    @ObservationIgnored private var observers: [UUID: () -> Void] = [:]
    @ObservationIgnored private var expiry: Timer?
    @ObservationIgnored private var closed = false
    /// Devices and passwords given for this run only (`--host`, `--fake`).
    @ObservationIgnored private var transient: [String: String] = [:]
    /// Opens a window (set by the app delegate).
    @ObservationIgnored var openWindow: (WindowContent) -> Void = { _ in }

    private(set) var devices: [DeviceRuntime] = []
    private(set) var layouts: [SavedLayout] = []
    private(set) var groups: [CameraGroup] = []
    private(set) var detections = DetectionTracker()
    private(set) var audible: Set<CameraRef> = []
    /// False when the core has no layouts store (an ABI 2 core): layouts
    /// then last only for this session.
    private(set) var layoutsStored = true
    /// The core's timing hooks, by `what` (nanoseconds).
    private(set) var timings: [String: UInt64] = [:]
    /// A message for the user (shown in the sidebar footer), if any.
    var notice: String?

    /// Gives the core's event thread a way back to the model on the main queue.
    private final class Target: @unchecked Sendable {
        // Touched only on the main queue.
        weak var model: AppModel?
    }

    init(
        factory: CoreFactory, fakeBox: FakeBox?, credentials: any CredentialStore,
        deviceFile: JSONFileStore<DeviceConfig>, groupFile: JSONFileStore<CameraGroup>
    ) throws {
        let target = Target()
        bridge = try CoreBridge(
            factory: factory,
            onEvent: { event in
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { target.model?.handle(event) }
                }
            })
        fake = fakeBox?.core
        leases = TileLeases(bridge: bridge)
        self.credentials = credentials
        self.deviceFile = deviceFile
        self.groupFile = groupFile
        target.model = self
        leases.resolve = { [weak self] key in self?.devices.first { $0.key == key }?.deviceID }
        leases.onTileChange = { [weak self] camera, tile in
            guard let self, let tile else { return }
            try? bridge.setTileAudio(tile, audible: audible.contains(camera))
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
            MainActor.assumeIsolated { self?.expireDetections() }
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

    // MARK: Devices

    /// By camera key or configuration key.
    func runtime(_ key: String) -> DeviceRuntime? { index(key).map { devices[$0] } }

    private func key(forDevice id: UInt64) -> String? { devices.first { $0.deviceID == id }?.key }

    private func index(_ key: String) -> Int? { devices.firstIndex { $0.key == key || $0.config.key == key } }

    func hasPassword(_ key: String) -> Bool { runtime(key).flatMap { password($0.config.key) } != nil }

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
            groups[g].cameras = groups[g].cameras.map { $0.device == old ? CameraRef(device: coreKey, channel: $0.channel) : $0 }
        }
        groupFile.save(groups)
        audible = Set(audible.map { $0.device == old ? CameraRef(device: coreKey, channel: $0.channel) : $0 })
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

    func name(of camera: CameraRef) -> String {
        if let name = channelInfo(camera)?.name, !name.isEmpty { return name }
        return "Channel \(camera.channel + 1)"
    }

    func deviceName(_ key: String) -> String { runtime(key)?.config.name ?? "Removed device" }

    /// Whether the camera detects objects itself (otherwise detection would
    /// come from this Mac).
    func hasCameraAI(_ camera: CameraRef) -> Bool { !(channelInfo(camera)?.detects.isEmpty ?? true) }

    /// Every camera of every device, in order.
    var allCameras: [CameraRef] {
        devices.flatMap { d in d.channels.map { CameraRef(device: d.key, channel: $0.channel) } }
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

    private func setAudible(_ new: Set<CameraRef>) {
        let changed = audible.symmetricDifference(new)
        audible = new
        for camera in changed {
            // Unmuting only routes audio already flowing in the core.
            if let tile = leases.tile(for: camera) {
                try? bridge.setTileAudio(tile, audible: new.contains(camera))
            }
        }
        notify()
    }

    /// The level meter's value for an audible camera.
    func level(_ camera: CameraRef) -> AudioLevel? {
        guard audible.contains(camera), let tile = leases.tile(for: camera) else { return nil }
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
            case .loginFailed:
                // Terminal: the core does not retry. The tiles end; new
                // credentials in Settings add the device again.
                detections.clear(device: key)
                leases.closeTiles(device: key)
            default:
                detections.clear(device: key)
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
        case .detection(let d):
            guard let key = key(forDevice: d.device) else { return }
            detections.apply(d, camera: CameraRef(device: key, channel: d.channel))
        case .layoutsChanged:
            reloadLayouts()
        case .timing(let what, _, let ns):
            if timings[what] == nil { timings[what] = ns }
        case .tileTier, .audioStats, .tileStats:
            break
        }
        notify()
    }

    private func expireDetections() {
        let before = detections
        // A stop lost to a reconnect must not leave a badge up for good.
        detections.expire(now: HostClock.nowNanos(), maxAgeNanos: 120_000_000_000)
        if detections != before { notify() }
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
