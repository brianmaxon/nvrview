import Foundation
import Synchronization
import VideoKit

/// Owns the core and carries out what it sends.
///
/// - Actions arrive per tile (`CoreSink.core(didDeliver:tile:)`), are routed
///   synchronously to that tile's `TileDecoder`, and the decoder's results
///   go straight back through `CoreAPI.tileDecoded` / `tileDecodeFailed`.
/// - Events (JSON) are parsed into `CoreEvent` and handed to `onEvent` on the
///   core's thread; hop to the main actor there if you touch UI.
///
/// Threading (docs/FFI.md): `addDevice`, `removeDevice`, `openTile`,
/// `closeTile` and `shutdown` must be called from outside any callback; this
/// class never calls them from one (the event handler must not either).
/// Late decoder results after a tile or the core is gone are dropped, never
/// sent to a freed core.
public final class CoreBridge: Sendable {
    public typealias EventHandler = @Sendable (CoreEvent) -> Void

    final class Router: CoreSink {
        struct Tiles {
            var byID: [UInt64: CoreTile] = [:]
        }
        struct Gate {
            var core: (any CoreAPI)?
            var closing = false
        }
        /// Held across `nv_tile_open`, so an action that arrives before the
        /// call returns (and before the tile's ID is known) waits for the
        /// registration instead of being lost.
        let registry = Mutex(Tiles())
        /// Protects calls into the core from decoder threads against
        /// `shutdown`: reports hold it (they never block), shutdown takes the
        /// core out through it before freeing.
        let gate = Mutex(Gate())
        let onEvent: EventHandler

        init(onEvent: @escaping EventHandler) { self.onEvent = onEvent }

        func core(didDeliver action: CoreAction, tile id: UInt64) {
            guard let tile = registry.withLock({ $0.byID[id] }) else { return }
            tile.perform(action)
        }

        func core(didEmit json: UnsafeRawBufferPointer) {
            guard let event = CoreEvent.parse(Data(json)) else { return }
            if case .tileClosed(let id, _) = event {
                // The core has ended it; nothing more may be reported. The
                // decoder stays until the app closes the tile (that call is
                // not allowed here).
                registry.withLock { $0.byID[id] }?.markClosed()
            }
            onEvent(event)
        }

        func reportDecoded(link: CoreTile.TileLink, tile: UInt64, ptsMicros: Int64) {
            gate.withLock { g in
                guard !g.closing, !link.closed.load(ordering: .acquiring) else { return }
                g.core?.tileDecoded(tile, ptsMicros: ptsMicros)
            }
        }

        func reportFailed(link: CoreTile.TileLink, tile: UInt64, _ kind: CoreDecodeFailure) {
            gate.withLock { g in
                guard !g.closing, !link.closed.load(ordering: .acquiring) else { return }
                g.core?.tileDecodeFailed(tile, kind: kind)
            }
        }
    }

    private let router: Router
    /// The core, for calls from outside callbacks. Nil after `shutdown`.
    private let core = Mutex<(any CoreAPI)?>(nil)

    /// Creates the core through `factory` (`CCore.factory(...)` or
    /// `FakeCore.factory(...)`).
    public init(factory: CoreFactory, onEvent: @escaping EventHandler = { _ in }) throws(CoreStatus) {
        router = Router(onEvent: onEvent)
        let created = try factory(router)
        router.gate.withLock { $0.core = created }
        core.withLock { $0 = created }
    }

    deinit { shutdown() }

    private func live() throws(CoreStatus) -> any CoreAPI {
        guard let c = core.withLock({ $0 }) else { throw .closed }
        return c
    }

    // MARK: Devices and tiles

    public func addDevice(host: String, port: UInt16, user: String, password: String) throws(CoreStatus) -> UInt64 {
        try live().addDevice(host: host, port: port, user: user, password: password)
    }

    /// Ends the device's tiles too; their decoders are released here.
    public func removeDevice(_ device: UInt64) throws(CoreStatus) {
        let c = try live()
        try c.removeDevice(device)
        let ended = router.registry.withLock { t -> [CoreTile] in
            let ids = t.byID.filter { $0.value.device == device }.map(\.key)
            return ids.compactMap { t.byID.removeValue(forKey: $0) }
        }
        for tile in ended { tile.markClosed() }
    }

    public func openTile(device: UInt64, channel: UInt8, tier: StreamTier) throws(CoreStatus) -> CoreTile {
        let c = try live()
        let tile = CoreTile(router: router, device: device, channel: channel, tier: tier)
        // Registration happens before the lock is released, so no action can
        // reach `Router.core(didDeliver:)` for this tile in between.
        let result: Result<Void, CoreStatus> = router.registry.withLock { t in
            do throws(CoreStatus) {
                let id = try c.openTile(device: device, channel: channel, tier: tier)
                tile.assign(id: id)
                t.byID[id] = tile
                return .success(())
            } catch {
                return .failure(error)
            }
        }
        if case .failure(let status) = result { throw status }
        return tile
    }

    /// Closes a tile and releases its decoder. No action for it runs after
    /// this returns.
    public func closeTile(_ tile: CoreTile) {
        guard let c = core.withLock({ $0 }) else { return }
        let id = tile.id
        // The result is irrelevant: unknown means the core already ended it.
        try? c.closeTile(id)
        tile.markClosed()
        router.registry.withLock { _ = $0.byID.removeValue(forKey: id) }
    }

    /// The tiles currently registered (open and not yet closed by the app).
    public var tiles: [CoreTile] { router.registry.withLock { Array($0.byID.values) } }

    /// `nv_grid_layout`; empty after `shutdown`.
    public func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool = false)
        -> [TileLayout]
    {
        guard let c = core.withLock({ $0 }) else { return [] }
        return (try? c.gridLayout(viewport: viewport, gap: gap, pictureSizes: pictureSizes, fill: fill)) ?? []
    }

    // MARK: Shutdown

    /// Closes everything: stops reporting, frees the core (no callback runs
    /// after that), then releases every decoder. Idempotent. Call from outside
    /// any callback, on every exit path.
    public func shutdown() {
        // Stops decoder threads from calling into the core, and waits for any
        // call in progress (they never block, so this is brief).
        let taken = router.gate.withLock { g -> (any CoreAPI)? in
            g.closing = true
            defer { g.core = nil }
            return g.core
        }
        core.withLock { $0 = nil }
        taken?.free()
        let tiles = router.registry.withLock { t -> [CoreTile] in
            defer { t.byID.removeAll() }
            return Array(t.byID.values)
        }
        for tile in tiles {
            tile.markClosed()
            tile.decoder.waitForPendingFrames()
        }
    }
}
