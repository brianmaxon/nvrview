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

    /// Minor 3 `nv_tile_retry`: the Retry button. The core restarts the
    /// tile's stream now, or connects now when its device is waiting to; it
    /// ignores a second ask within 2 s and does nothing for a refused login.
    public func retryTile(_ tile: CoreTile) throws(CoreStatus) {
        try live().tileRetry(tile.id)
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

    /// Minor 4 `nv_talk_set_conversation`: a call is open on the tile's
    /// camera (or no longer), so its talk starts skip the per-minute limit.
    public func setTalkConversation(_ tile: CoreTile, open: Bool) throws(CoreStatus) {
        try live().talkSetConversation(tile.id, open: open)
    }

    // MARK: Camera motion (minor 11)

    /// `nv_ptz_info` for the tile's camera. It waits for the camera (up to
    /// 20 s): never on the main thread.
    public func ptzInfo(_ tile: CoreTile) throws(CoreStatus) -> PTZInfo {
        let json = try live().ptzInfo(tile: tile.id)
        guard let info = PTZInfo.parse(json) else { throw .internal }
        return info
    }

    /// `nv_ptz_move`: starts a move, or renews it (call again at least
    /// every 500 ms while the control is held).
    public func ptzMove(_ tile: CoreTile, _ direction: PTZDirection, speed: Int) throws(CoreStatus) {
        try live().ptzMove(tile.id, direction: direction, speed: speed)
    }

    /// `nv_ptz_stop`.
    public func ptzStop(_ tile: CoreTile) throws(CoreStatus) { try live().ptzStop(tile.id) }

    /// `nv_ptz_lens`: an absolute position, clamped by the core to the range.
    public func ptzLens(_ tile: CoreTile, _ axis: PTZLensAxis, position: Int) throws(CoreStatus) {
        try live().ptzLens(tile.id, axis: axis, position: position)
    }

    /// `nv_ptz_preset`: recalls a preset (recall only).
    public func ptzPreset(_ tile: CoreTile, id: Int) throws(CoreStatus) { try live().ptzPreset(tile.id, id: id) }

    /// `nv_motion_grant`: lets an automation move one channel (passed again
    /// after each `addDevice`; the core keeps grants for this run only).
    public func motionGrant(device: UInt64, channel: UInt8, client: String, granted: Bool) throws(CoreStatus) {
        try live().motionGrant(device: device, channel: channel, client: client, granted: granted)
    }

    /// The core, for the recordings calls (minor 5; docs/FFI-RECORDINGS.md):
    /// they answer from memory and never wait on a device, so the main
    /// thread may make them. Not from inside a callback. `.closed` after
    /// `shutdown`.
    public func recordings() throws(CoreStatus) -> any CoreAPI { try live() }

    /// The core, for the capture calls (minor 9; docs/FFI.md "Capture"):
    /// they never wait on a device. Not from inside a callback, except
    /// `recordAudioEncoded`. `.closed` after `shutdown`.
    public func capture() throws(CoreStatus) -> any CoreAPI { try live() }

    /// The core, for the smart-alert calls (minor 12; docs/FFI.md "Smart
    /// alerts"): they never wait on a device. Not from inside a callback.
    /// `.closed` after `shutdown`.
    public func smartAlerts() throws(CoreStatus) -> any CoreAPI { try live() }

    /// `nv_clock_now_ns`.
    public func clockNowNanos() -> UInt64 { core.withLock { $0 }?.clockNowNanos() ?? HostClock.nowNanos() }

    // MARK: Recorded playback (minor 6)

    /// A playback the bridge opened: its ID and one tile per camera, in
    /// request order. Its tiles decode like live ones (the same actions,
    /// generations, and answers), so any renderer of `CoreTile` draws them.
    public struct OpenedTiles: Sendable {
        public let id: UInt64
        public let tiles: [CoreTile]
    }

    /// `nv_playback_open` (`PlaybackRequest` builds the JSON); `cameras` are
    /// the request's cameras in order (channel and tier, for the tiles).
    /// The tiles are registered before any of their actions can arrive.
    public func openPlayback(json: String, cameras: [(channel: UInt8, tier: StreamTier)]) throws(CoreStatus)
        -> OpenedTiles
    {
        let c = try live()
        let result: Result<OpenedTiles, CoreStatus> = router.registry.withLock { t in
            do throws(CoreStatus) {
                let opened = try c.playbackOpen(json: json)
                var tiles: [CoreTile] = []
                for (i, id) in opened.tiles.enumerated() {
                    let cam = i < cameras.count ? cameras[i] : (channel: 0, tier: .sub)
                    let tile = CoreTile(router: router, device: 0, channel: cam.channel, tier: cam.tier)
                    tile.markPlayback()
                    tile.assign(id: id)
                    t.byID[id] = tile
                    tiles.append(tile)
                }
                return .success(OpenedTiles(id: opened.playback, tiles: tiles))
            } catch {
                return .failure(error)
            }
        }
        switch result {
        case .success(let o): return o
        case .failure(let e): throw e
        }
    }

    /// `nv_playback_add_camera`: the new camera's tile, registered first.
    public func addPlaybackCamera(_ playback: UInt64, json: String, channel: UInt8, tier: StreamTier) throws(CoreStatus)
        -> CoreTile
    {
        let c = try live()
        let tile = CoreTile(router: router, device: 0, channel: channel, tier: tier)
        tile.markPlayback()
        let result: Result<Void, CoreStatus> = router.registry.withLock { t in
            do throws(CoreStatus) {
                let id = try c.playbackAddCamera(playback, json: json)
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

    /// `nv_playback_remove_camera`; the tile's decoder is released.
    public func removePlaybackCamera(_ playback: UInt64, tile: CoreTile) {
        if let c = core.withLock({ $0 }) { try? c.playbackRemoveCamera(playback, tile: tile.id) }
        forgetTiles([tile])
    }

    /// `nv_playback_close`: every play stops, every tile closes; their
    /// decoders are released. Unknown (already closed by the core) is fine.
    public func closePlayback(_ playback: UInt64, tiles: [CoreTile]) {
        if let c = core.withLock({ $0 }) { try? c.playbackClose(playback) }
        forgetTiles(tiles)
    }

    /// Releases tiles the core has already closed (`playback_closed`).
    public func forgetTiles(_ tiles: [CoreTile]) {
        for tile in tiles { tile.markClosed() }
        router.registry.withLock { t in
            for tile in tiles where t.byID[tile.id] === tile { t.byID.removeValue(forKey: tile.id) }
        }
    }

    /// `nv_playback_position`, or nil when the playback or core is gone.
    public func playbackPosition(_ playback: UInt64) -> PlaybackPosition? {
        guard let c = core.withLock({ $0 }) else { return nil }
        return try? c.playbackPosition(playback)
    }

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

