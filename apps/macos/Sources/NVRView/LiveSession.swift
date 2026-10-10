import AppKit
import FakeCore
import Foundation
import NVRCore
#if NVRVIEW_REAL_CORE
import NVRCoreC
#endif

/// Where connection settings come from and go to the core.
struct ConnectionSettings: Equatable {
    var host: String
    var port: UInt16
    var user: String
    var channel: UInt8
    var tier: StreamTier
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

/// The one live channel: a core (real or fake), its device, and its tile,
/// plus the status line's state. Main actor only; the core's events are
/// forwarded to the main queue in order.
@MainActor
final class LiveSession {
    /// Gives the core's event thread a way back to the session on the main queue.
    private final class Target: @unchecked Sendable {
        // Touched only on the main queue.
        weak var session: LiveSession?
    }

    let bridge: CoreBridge
    /// Set with `--fake`, for the self-test's contract checks.
    let fake: FakeCore?
    private(set) var tile: CoreTile?
    private(set) var status = ConnectionStatus()
    var onChange: () -> Void = {}
    private var device: UInt64?
    private var closed = false
    /// Plays the core's audio on the default output device.
    private var player: AudioPlayer?
    /// Whether the shown tile is audible (the Listen menu item).
    private(set) var listening = false

    /// The real core, if this build has one.
    static func realFactory() -> CoreFactory? {
        #if NVRVIEW_REAL_CORE
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Logs/NVRView", isDirectory: true)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        return CCore.factory(logDirectory: logs.path)
        #else
        return nil
        #endif
    }

    /// `fakeBox` is filled by a `FakeCore` factory (see `FakeBox`) and read
    /// once the core exists.
    init(factory: CoreFactory, fakeBox: FakeBox? = nil) throws {
        let target = Target()
        bridge = try CoreBridge(
            factory: factory,
            onEvent: { event in
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { target.session?.handle(event) }
                }
            })
        fake = fakeBox?.core
        target.session = self
    }

    /// Adds the device and opens its tile. A previous device is removed first.
    func connect(_ s: ConnectionSettings, password: String) throws {
        disconnect()
        status = ConnectionStatus()
        onChange()
        let id = try bridge.addDevice(host: s.host, port: s.port, user: s.user, password: password)
        device = id
        tile = try bridge.openTile(device: id, channel: s.channel, tier: s.tier)
        applyListening()
        onChange()
    }

    /// Removes the device (its tile ends with it). Idempotent.
    func disconnect() {
        guard let id = device else { return }
        if let tile { bridge.closeTile(tile) }
        try? bridge.removeDevice(id)
        device = nil
        tile = nil
    }

    /// Starts audio output on the default device. `muted` silences the
    /// device side (self-tests never make a sound).
    func startAudio(muted: Bool) {
        guard player == nil, !closed else { return }
        do {
            let p = try bridge.makeAudioPlayer()
            if muted { p.outputVolume = 0 }
            try p.start()
            player = p
        } catch {
            FileHandle.standardError.write(Data("audio output unavailable: \(error)\n".utf8))
        }
    }

    /// The Listen toggle: makes the shown tile audible or silent.
    func setListening(_ on: Bool) {
        listening = on
        applyListening()
        onChange()
    }

    private func applyListening() {
        guard let tile else { return }
        try? bridge.setTileAudio(tile, audible: listening)
    }

    /// Closes the core: every session, every tile. Call on every exit path.
    func shutdown() {
        guard !closed else { return }
        closed = true
        // Audio renders straight from the core: stop it first (the bridge
        // would too).
        player?.stop()
        player = nil
        bridge.shutdown()
        tile = nil
        device = nil
    }

    /// Refreshes what comes from the tile rather than from events.
    func refresh() {
        if let tile, status.tile != tile.snapshot.status {
            status.tile = tile.snapshot.status
            onChange()
        }
    }

    private func handle(_ event: CoreEvent) {
        guard !closed else { return }
        // Events can arrive before `addDevice` has returned the ID (nothing
        // to compare with yet); afterwards only this device's count.
        switch event {
        case .deviceState(let d, _), .deviceInfo(let d, _, _), .channels(let d, _):
            if let device, d != device { return }
        case .tileClosed(let t, _):
            if let tile, t != tile.id { return }
        case .tileStats, .tileTier, .detection, .layoutsChanged, .timing:
            return
        case .audioStats(let t, _):
            if let tile, t != tile.id { return }
        }
        status.apply(event)
        onChange()
    }
}
