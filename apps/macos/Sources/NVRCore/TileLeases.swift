import Foundation

/// One core tile per camera, shared by every view that shows it (docs/UX.md:
/// the same camera in several tiles and windows is streamed once).
///
/// Views `acquire` the cameras they show and `release` them when they stop;
/// the tile is opened on the first lease and closed with the last. A camera
/// focused in any view is switched to `focusTier` through the tier-switch
/// call (the picture sharpens in place) and back to `gridTier` when no view
/// focuses it any more.
///
/// The second lens of a multi-lens camera (`CameraRef.lens` 1) is its own
/// camera here, with its own tile, on the same channel's lens-2 tiers:
/// `mobile` in a grid and `extern` when focused. Like every tile it opens
/// on the stream it should show, so a grid tile starts on the low tier
/// (`mobile`), never on the high one; if the device refuses `mobile` (the
/// start or a later switch fails), the camera uses `extern` for the rest of
/// the run and its tile is reopened on it.
///
/// Quality (docs/UX.md "Stream quality per tile"): every tile showing a camera
/// may state a demand (`setDemand`): its quality choice, whether it is
/// focused, and its zoom. Each demand maps to a stream level (`low`, `mid`,
/// `high`), and the shared tile runs at the highest level any demand or
/// focus asks for, through the aligned tier switch:
///
/// - Auto: `high` while focused or zoomed in (up at 2x, down at 1.6x),
///   else `low`.
/// - Clear: `high`, also when not focused.
/// - Fluent: `low`, also when focused. An explicit choice outranks focus.
/// - Balanced: `mid` (the single-lens `extern`), also when focused; it is
///   offered only for a first lens whose channel lists `extern`
///   (`hasMiddleTier`), and without that it acts as Auto.
///
/// Focus without a demand (`setFocused`) counts as `high`. The levels map
/// to tiers per camera: a first lens `gridTier` / `extern` / `focusTier`; a
/// second lens `mobile` (or `extern` once `mobile` is refused) / (none) /
/// `extern`.
///
/// Use from one thread (the main thread in the app): every call here may
/// open or close tiles, which must not happen inside a core callback.
public final class TileLeases {
    private struct Entry {
        var tile: CoreTile?
        var leases = 0
        var focus = 0
        /// The tier the tile was opened on.
        var openedTier: StreamTier?
    }

    /// What one tile (one owner: a window's tile) asks of its camera.
    public struct Demand: Sendable, Equatable {
        public var quality: TileQuality
        public var focused: Bool
        /// Auto's zoom state, kept between calls for the hysteresis.
        public var zoomHigh: Bool

        public init(quality: TileQuality = .auto, focused: Bool = false, zoomHigh: Bool = false) {
            self.quality = quality
            self.focused = focused
            self.zoomHigh = zoomHigh
        }
    }

    private let bridge: CoreBridge
    private var demands: [AnyHashable: (camera: CameraRef, demand: Demand)] = [:]
    private var entries: [CameraRef: Entry] = [:]
    /// Second lenses whose `mobile` tier failed: they stay on `extern`.
    private var noMobile: Set<CameraRef> = []

    /// The stream a tile shows in a grid, and when focused.
    public var gridTier: StreamTier
    public var focusTier: StreamTier
    /// The per-run device ID for a device key, nil while it has none (not
    /// added yet, or removed).
    public var resolve: (String) -> UInt64? = { _ in nil }
    /// Whether a camera serves a middle stream to offer as Balanced: a first
    /// lens whose channel's tiers list `extern`. Set by the app.
    public var hasMiddleTier: (CameraRef) -> Bool = { _ in false }
    /// Called whenever a tile is opened (so audio settings can be applied
    /// to it) and with nil when one is closed.
    public var onTileChange: (CameraRef, CoreTile?) -> Void = { _, _ in }

    public init(bridge: CoreBridge, gridTier: StreamTier = .sub, focusTier: StreamTier = .main) {
        self.bridge = bridge
        self.gridTier = gridTier
        self.focusTier = focusTier
    }

    /// Every camera with at least one lease.
    public var cameras: Set<CameraRef> { Set(entries.keys) }

    /// The tile showing `camera`, if it is open.
    public func tile(for camera: CameraRef) -> CoreTile? { entries[camera]?.tile }

    /// The camera a tile shows.
    public func camera(forTile id: UInt64) -> CameraRef? {
        entries.first { $0.value.tile?.id == id }?.key
    }

    public func leaseCount(_ camera: CameraRef) -> Int { entries[camera]?.leases ?? 0 }

    /// Takes a lease; opens the tile if this is the first (and the device is
    /// known). Returns the tile, nil while it cannot be opened.
    @discardableResult
    public func acquire(_ camera: CameraRef) -> CoreTile? {
        entries[camera, default: Entry()].leases += 1
        return open(camera)
    }

    /// Gives a lease back; the last one closes the tile.
    public func release(_ camera: CameraRef) {
        guard var e = entries[camera] else { return }
        e.leases -= 1
        if e.leases > 0 {
            entries[camera] = e
            return
        }
        entries[camera] = nil
        demands = demands.filter { $0.value.camera != camera }
        if let tile = e.tile {
            bridge.closeTile(tile)
            onTileChange(camera, nil)
        }
    }

    /// Makes the leases match `wanted` exactly for one owner that held `held`:
    /// acquires what is new, releases what went away.
    public func update(from held: [CameraRef: Int], to wanted: [CameraRef: Int]) {
        for (camera, n) in wanted {
            let extra = n - (held[camera] ?? 0)
            if extra > 0 { for _ in 0..<extra { acquire(camera) } }
        }
        for (camera, n) in held {
            let fewer = n - (wanted[camera] ?? 0)
            if fewer > 0 { for _ in 0..<fewer { release(camera) } }
        }
    }

    /// A view focuses (or stops focusing) `camera`.
    public func setFocused(_ camera: CameraRef, _ focused: Bool) {
        guard var e = entries[camera] else { return }
        let before = e.focus > 0
        e.focus = max(e.focus + (focused ? 1 : -1), 0)
        entries[camera] = e
        if before != (e.focus > 0) { applyTier(camera) }
    }

    public func isFocused(_ camera: CameraRef) -> Bool { (entries[camera]?.focus ?? 0) > 0 }

    /// The tiers a camera uses in a grid and when focused.
    public func tiers(for camera: CameraRef) -> (grid: StreamTier, focus: StreamTier) {
        guard camera.lens > 0 else { return (gridTier, focusTier) }
        return (noMobile.contains(camera) ? .extern : .mobile, .extern)
    }

    /// The tier `camera` should show now: the highest level asked for.
    public func wantedTier(_ camera: CameraRef) -> StreamTier {
        tier(for: wantedLevel(camera), camera)
    }

    /// The highest level any focus or demand asks of `camera`.
    public func wantedLevel(_ camera: CameraRef) -> StreamLevel {
        var level: StreamLevel = isFocused(camera) ? .high : .low
        for d in demands.values where d.camera == camera { level = max(level, self.level(of: d.demand, camera)) }
        return level
    }

    /// What one demand asks of `camera`.
    public func level(of demand: Demand, _ camera: CameraRef) -> StreamLevel {
        switch effectiveQuality(demand.quality, camera) {
        case .clear: return .high
        case .fluent: return .low
        case .balanced: return .mid
        case .auto: return demand.focused || demand.zoomHigh ? .high : .low
        }
    }

    /// The quality a tile can actually have: Balanced only where offered.
    public func effectiveQuality(_ quality: TileQuality, _ camera: CameraRef) -> TileQuality {
        quality == .balanced && !offersBalanced(camera) ? .auto : quality
    }

    /// Whether Balanced is offered for `camera`: a first lens with `extern`.
    public func offersBalanced(_ camera: CameraRef) -> Bool { camera.lens == 0 && hasMiddleTier(camera) }

    /// The tier a level means for `camera`.
    public func tier(for level: StreamLevel, _ camera: CameraRef) -> StreamTier {
        if camera.lens > 0 {
            return level == .low && !noMobile.contains(camera) ? .mobile : .extern
        }
        switch level {
        case .low: return gridTier
        case .mid: return .extern
        case .high: return focusTier
        }
    }

    /// A tile (`owner`) states what it asks of `camera`: its quality choice,
    /// whether it is focused, and its zoom scale (1 when not zoomed). The
    /// hysteresis of Auto's zoom rule lives here. Idempotent; call it
    /// whenever any of them changes.
    public func setDemand(
        owner: AnyHashable, camera: CameraRef, quality: TileQuality, focused: Bool, zoomScale: Double
    ) {
        let was = demands[owner]
        let wasHigh = was?.camera == camera && was?.demand.zoomHigh == true
        let demand = Demand(
            quality: quality, focused: focused, zoomHigh: ZoomHysteresis.next(high: wasHigh, scale: zoomScale))
        demands[owner] = (camera, demand)
        applyTier(camera)
        if let old = was?.camera, old != camera { applyTier(old) }
    }

    /// Withdraws everything `owner` asked for (its tile went away).
    public func clearDemands(owner: AnyHashable) {
        guard let old = demands.removeValue(forKey: owner) else { return }
        applyTier(old.camera)
    }

    /// The demand `owner` last stated.
    public func demand(owner: AnyHashable) -> Demand? { demands[owner]?.demand }

    /// A tier switch failed (`tile_tier` `failed`). For a second lens whose
    /// `mobile` failed for any reason but being superseded, the grid uses
    /// `extern` from now on (the old tier keeps showing meanwhile).
    public func tierFailed(tile id: UInt64, tier: StreamTier?, reason: String?) {
        guard let camera = camera(forTile: id), camera.lens > 0, tier == .mobile, reason != "superseded" else {
            return
        }
        noMobile.insert(camera)
        applyTier(camera, force: true)
    }

    /// A tile could not start its stream (`tile_error`). A second lens whose
    /// `mobile` start was refused uses `extern` from now on: its tile is
    /// closed and reopened on it. Other failures are left to the device's
    /// reconnect handling.
    public func startFailed(tile id: UInt64, reason: String?) {
        guard let camera = camera(forTile: id), camera.lens > 0, reason == "start_rejected",
            entries[camera]?.openedTier == .mobile, let tile = entries[camera]?.tile
        else { return }
        noMobile.insert(camera)
        bridge.closeTile(tile)
        entries[camera]?.tile = nil
        onTileChange(camera, nil)
        open(camera)
    }

    /// Closes a device's tiles (it is being removed or re-added); their
    /// leases stay, so `reopen` brings them back.
    public func closeTiles(device key: String) {
        for (camera, e) in entries where camera.device == key {
            guard let tile = e.tile else { continue }
            bridge.closeTile(tile)
            entries[camera]?.tile = nil
            onTileChange(camera, nil)
        }
    }

    /// Opens the tiles of a device that has (again) a device ID.
    public func reopen(device key: String) {
        for camera in entries.keys where camera.device == key { open(camera) }
    }

    /// The core ended a tile on its own (`tile_closed`): release our side.
    /// It is reopened by `reopen` once its device is back.
    public func tileEnded(_ id: UInt64) {
        guard let camera = camera(forTile: id), let tile = entries[camera]?.tile else { return }
        bridge.closeTile(tile)
        entries[camera]?.tile = nil
        onTileChange(camera, nil)
    }

    /// Closes every tile and forgets every lease.
    public func closeAll() {
        for (camera, e) in entries {
            if let tile = e.tile {
                bridge.closeTile(tile)
                onTileChange(camera, nil)
            }
        }
        entries.removeAll()
        demands.removeAll()
    }

    @discardableResult
    private func open(_ camera: CameraRef) -> CoreTile? {
        guard let e = entries[camera] else { return nil }
        if let tile = e.tile { return tile }
        guard let device = resolve(camera.device), (0...255).contains(camera.channel) else { return nil }
        // Open on the tier the camera should show now: the low tier in a
        // grid (sub, or a second lens's mobile), never the high one first.
        let tier = wantedTier(camera)
        guard let tile = try? bridge.openTile(device: device, channel: UInt8(camera.channel), tier: tier) else {
            return nil
        }
        entries[camera]?.tile = tile
        entries[camera]?.openedTier = tier
        onTileChange(camera, tile)
        return tile
    }

    private func applyTier(_ camera: CameraRef, force: Bool = false) {
        guard entries[camera] != nil, let tile = entries[camera]?.tile else { return }
        let tier = wantedTier(camera)
        // Asking again for the tier already asked for changes nothing.
        if !force, tile.tierState.requested == tier { return }
        // A core without the tier switch keeps the stream it has.
        try? bridge.setTileTier(tile, tier)
    }
}
