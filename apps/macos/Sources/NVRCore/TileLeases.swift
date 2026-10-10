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
/// Use from one thread (the main thread in the app): every call here may
/// open or close tiles, which must not happen inside a core callback.
public final class TileLeases {
    private struct Entry {
        var tile: CoreTile?
        var leases = 0
        var focus = 0
    }

    private let bridge: CoreBridge
    private var entries: [CameraRef: Entry] = [:]

    /// The stream a tile shows in a grid, and when focused.
    public var gridTier: StreamTier
    public var focusTier: StreamTier
    /// The per-run device ID for a device key, nil while it has none (not
    /// added yet, or removed).
    public var resolve: (String) -> UInt64? = { _ in nil }
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
    }

    @discardableResult
    private func open(_ camera: CameraRef) -> CoreTile? {
        guard let e = entries[camera] else { return nil }
        if let tile = e.tile { return tile }
        guard let device = resolve(camera.device), (0...255).contains(camera.channel) else { return nil }
        let tier = e.focus > 0 ? focusTier : gridTier
        guard let tile = try? bridge.openTile(device: device, channel: UInt8(camera.channel), tier: tier) else {
            return nil
        }
        entries[camera]?.tile = tile
        onTileChange(camera, tile)
        return tile
    }

    private func applyTier(_ camera: CameraRef) {
        guard let e = entries[camera], let tile = e.tile else { return }
        let tier = e.focus > 0 ? focusTier : gridTier
        // A core without the tier switch keeps the stream it has.
        try? bridge.setTileTier(tile, tier)
    }
}
