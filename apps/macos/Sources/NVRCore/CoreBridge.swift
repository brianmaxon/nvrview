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
    /// Receives tapped audio on the core's tap thread; the block is borrowed
    /// for the duration of the call.
    public typealias AudioTapHandler = @Sendable (_ block: CoreAudioBlock, _ tile: UInt64) -> Void

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
        let onAudioTap: AudioTapHandler?

        init(onEvent: @escaping EventHandler, onAudioTap: AudioTapHandler?) {
            self.onEvent = onEvent
            self.onAudioTap = onAudioTap
        }

        func core(didTap block: CoreAudioBlock, tile id: UInt64) {
            onAudioTap?(block, id)
        }

        func core(didDeliver action: CoreAction, tile id: UInt64) {
            core(didDeliver: action, tile: id, generation: 0)
        }

        func core(didDeliver action: CoreAction, tile id: UInt64, generation: UInt32) {
            guard let tile = registry.withLock({ $0.byID[id] }) else { return }
            tile.perform(action, generation: generation)
        }

        func core(didReject failure: CoreDecodeFailure, tile id: UInt64) {
            core(didReject: failure, tile: id, generation: 0)
        }

        func core(didReject failure: CoreDecodeFailure, tile id: UInt64, generation: UInt32) {
            registry.withLock { $0.byID[id] }?.reject(failure, generation: generation)
        }

        func core(didEmit json: UnsafeRawBufferPointer) {
            guard let event = CoreEvent.parse(Data(json)) else { return }
            if case .tileClosed(let id, _) = event {
                // The core has ended it; nothing more may be reported. The
                // decoder stays until the app closes the tile (that call is
                // not allowed here).
                registry.withLock { $0.byID[id] }?.markClosed()
            }
            if case .tileTier(let id, let tier, let state, let generation, let reason) = event {
                // Applied here, on the core's thread, so the tile shows the
                // new generation from the next refresh on, whatever the app's
                // main thread is doing.
                registry.withLock { $0.byID[id] }?.applyTier(tier, state: state, generation: generation, reason: reason)
            }
            onEvent(event)
        }

        func reportDecoded(link: CoreTile.TileLink, tile: UInt64, ptsMicros: Int64) {
            gate.withLock { g in
                guard !g.closing, !link.closed.load(ordering: .acquiring) else { return }
                g.core?.tileDecoded(tile, generation: link.generation, ptsMicros: ptsMicros)
            }
        }

        func reportFailed(link: CoreTile.TileLink, tile: UInt64, _ kind: CoreDecodeFailure) {
            gate.withLock { g in
                guard !g.closing, !link.closed.load(ordering: .acquiring) else { return }
                g.core?.tileDecodeFailed(tile, generation: link.generation, kind: kind)
            }
        }
    }

    private let router: Router
    /// The core, for calls from outside callbacks. Nil after `shutdown`.
    private let core = Mutex<(any CoreAPI)?>(nil)
    /// Players rendering from the core; stopped before it is freed.
    private let players = Mutex<[Weak<AudioPlayer>]>([])

    /// Creates the core through `factory` (`CCore.factory(...)` or
    /// `FakeCore.factory(...)`).
    public init(
        factory: CoreFactory, onEvent: @escaping EventHandler = { _ in }, onAudioTap: AudioTapHandler? = nil
    ) throws(CoreStatus) {
        router = Router(onEvent: onEvent, onAudioTap: onAudioTap)
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

    /// ABI 3 `nv_tile_set_tier`: the tile keeps showing its current stream
    /// until the new tier's first picture has decoded, then switches (the
    /// `tile_tier` event; `CoreTile.tierState` follows it).
    public func setTileTier(_ tile: CoreTile, _ tier: StreamTier) throws(CoreStatus) {
        let c = try live()
        tile.noteRequested(tier)
        do throws(CoreStatus) {
            try c.tileSetTier(tile.id, tier: tier)
        } catch {
            tile.noteRequested(tile.tier)
            throw error
        }
    }

    /// ABI 3 `nv_device_key`: the device's stable key, the name layouts use
    /// for it; nil when the core has none (an older core).
    public func deviceKey(_ device: UInt64) -> String? {
        guard let c = core.withLock({ $0 }) else { return nil }
        return try? c.deviceKey(device)
    }

    /// ABI 3 `nv_tile_audio_level`, or nil when the core cannot say.
    public func audioLevel(_ tile: CoreTile) -> AudioLevel? {
        guard let c = core.withLock({ $0 }) else { return nil }
        return try? c.tileAudioLevel(tile.id)
    }

    // MARK: Layouts (ABI 3)

    /// `nv_layouts_get`: every stored layout as JSON.
    public func layoutsJSON() throws(CoreStatus) -> String { try live().layoutsGet() }

    /// `nv_layout_put`: inserts or replaces one layout by its id.
    public func putLayout(json: String) throws(CoreStatus) { try live().layoutPut(json: json) }

    /// `nv_layout_delete`.
    public func deleteLayout(id: String) throws(CoreStatus) { try live().layoutDelete(id: id) }

    /// `nv_grid_layout`; empty after `shutdown`.
    public func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool = false)
        -> [TileLayout]
    {
        guard let c = core.withLock({ $0 }) else { return [] }
        return (try? c.gridLayout(viewport: viewport, gap: gap, pictureSizes: pictureSizes, fill: fill)) ?? []
    }

    // MARK: Discovery (minor 1)

    /// `nv_discover_start`: answers arrive as `discovered` events. Call from
    /// outside any callback.
    public func discoverStart(seconds: Double? = nil) throws(CoreStatus) {
        let json = seconds.map { "{\"seconds\":\($0)}" }
        try live().discoverStart(json: json)
    }

    /// `nv_discover_stop`; nothing running is fine.
    public func discoverStop() {
        guard let c = core.withLock({ $0 }) else { return }
        try? c.discoverStop()
    }

    // MARK: Updates (minor 2)

    /// `nv_update_check` with the request JSON (`UpdateRequest`).
    public func updateCheck(json: String) throws(CoreStatus) { try live().updateCheck(json: json) }

    /// `nv_update_download` with the request JSON.
    public func updateDownload(json: String) throws(CoreStatus) { try live().updateDownload(json: json) }

    /// `nv_update_cancel`; nothing running is fine.
    public func updateCancel() {
        guard let c = core.withLock({ $0 }) else { return }
        try? c.updateCancel()
    }

    /// `nv_update_parse_key`: `{"key":...,"id":...}` or nil.
    public func updateParseKey(_ text: String) -> String? { core.withLock({ $0 })?.updateParseKey(text) }

    /// `nv_update_parse_source`.
    public func updateParseSource(_ text: String) -> String? { core.withLock({ $0 })?.updateParseSource(text) }

    // MARK: Audio

    /// A player that renders this core's audio (`AudioPlayer`); it is stopped
    /// by `shutdown` before the core is freed. Start it yourself.
    public func makeAudioPlayer(mode: AudioPlayer.Mode = .device) throws(CoreStatus) -> AudioPlayer {
        let c = try live()
        let player = AudioPlayer(hook: c.audioRenderHook, mode: mode) { [weak self] rate, channels in
            guard let self else { return .closed }
            do throws(CoreStatus) {
                try self.live().audioSetOutput(sampleRate: rate, channels: channels)
                return .ok
            } catch {
                return error
            }
        }
        players.withLock { list in
            list.removeAll { $0.value == nil }
            list.append(Weak(player))
        }
        return player
    }

    /// `nv_tile_audio_set`: makes the tile audible (or silent) at `volume`.
    public func setTileAudio(_ tile: CoreTile, audible: Bool, volume: Float = 1) throws(CoreStatus) {
        try live().tileAudioSet(tile.id, audible: audible, volume: volume)
    }

    /// `nv_tile_audio_tap`: blocks go to the `onAudioTap` handler.
    public func setTileAudioTap(_ tile: CoreTile, enabled: Bool) throws(CoreStatus) {
        try live().tileAudioTap(tile.id, enabled: enabled)
    }

    // MARK: Talk-back (camera motion control)

    /// `nv_talk_set_permission` for one channel of a device (this run).
    public func setTalkPermission(device: UInt64, channel: UInt8, allowed: Bool) throws(CoreStatus) {
        try live().talkSetPermission(device: device, channel: channel, allowed: allowed)
    }

    /// `nv_talk_start` for the tile's camera; progress comes as `talk` events.
    public func startTalk(_ tile: CoreTile) throws(CoreStatus) { try live().talkStart(tile.id) }

    /// `nv_talk_push`: mono samples at `sampleRate`. Any thread but a
    /// real-time audio callback.
    public func pushTalk(_ tile: CoreTile, samples: [Float], sampleRate: UInt32) throws(CoreStatus) {
        let c = try live()
        let id = tile.id
        let result: Result<Void, CoreStatus> = samples.withUnsafeBufferPointer { buf in
            do throws(CoreStatus) {
                try c.talkPush(id, samples: buf, sampleRate: sampleRate)
                return .success(())
            } catch {
                return .failure(error)
            }
        }
        if case .failure(let status) = result { throw status }
    }

    /// `nv_talk_stop`: at once, or once what was pushed has been sent.
    public func stopTalk(_ tile: CoreTile, drain: Bool) throws(CoreStatus) { try live().talkStop(tile.id, drain: drain) }

    /// `nv_clock_now_ns`.
    public func clockNowNanos() -> UInt64 { core.withLock { $0 }?.clockNowNanos() ?? HostClock.nowNanos() }

    // MARK: Shutdown

    /// Closes everything: stops reporting, frees the core (no callback runs
    /// after that), then releases every decoder. Idempotent. Call from outside
    /// any callback, on every exit path.
    public func shutdown() {
        // Audio renders straight into the core: stop every player first.
        for player in players.withLock({ list in defer { list.removeAll() }; return list.compactMap(\.value) }) {
            player.stop()
        }
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
            tile.waitForPendingFrames()
        }
    }
}

