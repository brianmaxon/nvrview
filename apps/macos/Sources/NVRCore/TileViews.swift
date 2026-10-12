import CoreGraphics
import Foundation

/// How one tile's picture is shown (docs/UX.md "Digital zoom", "Fill
/// Tiles"): its digital zoom, or at 1x where its filled crop sits, and its
/// own Fill choice when it differs from the window's.
public struct TileView: Equatable, Sendable {
    public var zoom: TileZoom
    /// The tile's own Fill choice; nil follows the window's.
    public var fill: Bool?

    public init(zoom: TileZoom = .none, fill: Bool? = nil) {
        self.zoom = zoom
        self.fill = fill
    }
}

/// A window's picture settings per key (docs/UX.md "Fill Tiles"): the
/// window's Fill Tiles switch and, per key, the zoom or crop position and a
/// Fill choice of its own. The app keys it by tile (`TileKey`: the layout,
/// the tile, and the camera the tile shows), so two tiles of one camera are
/// independent views; another window has its own. Positions are fractions
/// of the picture, so the sub-stream and the main stream show the same
/// part.
///
/// The rule for Fill: a tile fills if it has its own choice, else as the
/// window says. A choice equal to the window's is not kept, and setting the
/// window's switch clears every tile's choice (all tiles follow it). At most
/// `limit` keys are kept; the one changed longest ago goes first. Pure; the
/// vectors in `fixtures/synthetic/tile-views.json` are shared with the
/// Windows app's TileViews (NvrView.Tiles).
public struct TileViews<Key: Hashable & Sendable>: Equatable, Sendable {
    /// The most tiles one window keeps.
    public static var limit: Int { 256 }

    /// The window's Fill Tiles switch: what tiles without a choice of
    /// their own do.
    public private(set) var fill: Bool
    public private(set) var entries: [Key: TileView] = [:]
    /// The keys kept, the one changed longest ago first.
    public private(set) var order: [Key] = []

    public init(fill: Bool = false) { self.fill = fill }

    /// The key's zoom (1x and centered when nothing is kept).
    public func zoom(_ key: Key) -> TileZoom { entries[key]?.zoom ?? .none }

    /// Whether the key's tile fills: its own choice, else the window's.
    public func fills(_ key: Key) -> Bool { entries[key]?.fill ?? fill }

    /// The key's own Fill choice (nil: it follows the window).
    public func fillChoice(_ key: Key) -> Bool? { entries[key]?.fill }

    /// Some key has a Fill choice of its own (the window's switch then
    /// shows mixed where the platform can).
    public var mixed: Bool { entries.values.contains { $0.fill != nil } }

    public var count: Int { order.count }

    public mutating func setZoom(_ zoom: TileZoom, for key: Key) {
        var v = entries[key] ?? TileView()
        v.zoom = zoom
        put(v, for: key)
    }

    /// The key's tile fills, or fits (a choice equal to the window's is not
    /// kept).
    public mutating func setFill(_ fills: Bool, for key: Key) {
        var v = entries[key] ?? TileView()
        v.fill = fills == fill ? nil : fills
        put(v, for: key)
    }

    /// The window's switch: every tile follows it (the tiles' own choices
    /// are cleared; their positions stay).
    public mutating func setWindowFill(_ fills: Bool) {
        fill = fills
        for key in order {
            guard var v = entries[key], v.fill != nil else { continue }
            v.fill = nil
            entries[key] = v.zoom.isDefault ? nil : v
        }
        order.removeAll { entries[$0] == nil }
    }

    /// Forgets the key (1x, centered, following the window).
    public mutating func remove(_ key: Key) {
        entries[key] = nil
        order.removeAll { $0 == key }
    }

    /// Forgets every key `drop` picks.
    public mutating func removeAll(where drop: (Key) -> Bool) {
        for key in order where drop(key) { entries[key] = nil }
        order.removeAll { entries[$0] == nil }
    }

    /// Moves views to new keys at once (`moves` maps old to new; a key
    /// moved away is left empty unless another moves onto it). The moved
    /// views become the newest.
    public mutating func move(_ moves: [(from: Key, to: Key)]) {
        let taken = moves.map { (entries[$0.from], $0.to) }
        for m in moves { remove(m.from) }
        for (view, to) in taken {
            remove(to)
            if let view { put(view, for: to) }
        }
    }

    /// Sets a key's whole view as read or migrated: clamped, and the choice
    /// kept only where it differs from the window's.
    mutating func restore(_ view: TileView, for key: Key) {
        var v = view
        v.zoom = v.zoom.sanitized
        if v.fill == fill { v.fill = nil }
        put(v, for: key)
    }

    private mutating func put(_ v: TileView, for key: Key) {
        order.removeAll { $0 == key }
        guard !v.zoom.isDefault || v.fill != nil else {
            entries[key] = nil
            return
        }
        entries[key] = v
        order.append(key)
        while order.count > Self.limit { entries[order.removeFirst()] = nil }
    }

    /// The kept keys, the one changed longest ago first (what is saved).
    public var saved: [(key: Key, view: TileView)] { order.compactMap { k in entries[k].map { (k, $0) } } }

    /// Read back from `saved` entries (oldest first): the values clamped,
    /// entries with nothing to keep dropped, a key listed twice taken from
    /// its later entry, and at most `limit` kept (the newest).
    public init(fill: Bool, saved: [(key: Key, view: TileView)]) {
        self.fill = fill
        for (key, view) in saved { restore(view, for: key) }
    }
}

// MARK: Tiles

/// Which tile a view belongs to: the window's content (`layout`, named as
/// `TileKey.layoutKey` does), the tile's identity in that layout
/// (`LayoutNode.tileID`), and the camera the tile shows. A tile given
/// another camera has a new key, so its view starts at the default; a swap
/// moves each view with its camera (`TileViews.swapViews`).
public struct TileKey: Hashable, Sendable, CustomStringConvertible {
    public var layout: String
    public var tile: String
    public var camera: CameraRef

    public init(layout: String, tile: String, camera: CameraRef) {
        self.layout = layout
        self.tile = tile
        self.camera = camera
    }

    public var description: String { "\(layout)#\(tile)@\(camera)" }

    /// The views key of a window's content: a stored layout, a group's grid,
    /// or the window's unsaved arrangement.
    public static func layoutKey(layout id: String) -> String { "layout:" + id }
    public static func layoutKey(group id: String) -> String { "group:" + id }
    public static let scratchKey = "scratch"
}

extension LayoutNode {
    /// The leaf member holding the tile's identity (stored with the layout;
    /// a member of the tile, so it stays in place through swaps and camera
    /// changes, and with the old tile through a split).
    public static let tileMember = "tile"

    /// The leaf's tile identity (nil for a split or a leaf without one).
    public var tileID: String? {
        guard case .leaf(_, let extras) = self, case .string(let s)? = extras.node[Self.tileMember],
            Self.validTileID(s)
        else { return nil }
        return s
    }

    static func validTileID(_ s: String) -> Bool { !s.isEmpty && s.count <= 64 }

    /// Identities for tiles in reading order, given the ones they have (nil:
    /// none, or one that cannot be used): each valid identity is kept the
    /// first time it appears; every other tile gets `t<n>` for its place
    /// `n` in reading order, or else the first free `t<n>.<k>` (k = 1, 2,
    /// ...). The same input gives the same identities, so a layout stored
    /// before tiles had identities reads the same way on every launch.
    /// Shared vectors: `tileIDs` in `fixtures/synthetic/tile-views.json`.
    public static func assignTileIDs(_ ids: [String?]) -> [String] {
        var used = Set<String>()
        var kept: [String?] = []
        for id in ids {
            if let id, validTileID(id), !used.contains(id) {
                used.insert(id)
                kept.append(id)
            } else {
                kept.append(nil)
            }
        }
        return kept.enumerated().map { i, id in
            if let id { return id }
            var candidate = "t\(i)"
            var k = 0
            while used.contains(candidate) {
                k += 1
                candidate = "t\(i).\(k)"
            }
            used.insert(candidate)
            return candidate
        }
    }

    /// The tree with every leaf holding a tile identity of its own
    /// (`assignTileIDs`); the same tree when they all do.
    public var withTileIDs: LayoutNode {
        let leaves = self.leaves
        let have = leaves.map { self[$0.path]?.tileID }
        let ids = Self.assignTileIDs(have)
        var tree = self
        for (i, leaf) in leaves.enumerated() where have[i] != ids[i] {
            guard case .leaf(let camera, var extras)? = tree[leaf.path] else { continue }
            extras.node[Self.tileMember] = .string(ids[i])
            tree = tree.replacing(at: leaf.path, with: .leaf(camera, extras: extras)) ?? tree
        }
        return tree
    }

    /// The view key of the tile at `path` in the content `layout`; nil
    /// without a camera or an identity.
    public func tileKey(at path: NodePath, layout: String) -> TileKey? {
        guard let node = self[path], let camera = node.camera, let id = node.tileID else { return nil }
        return TileKey(layout: layout, tile: id, camera: camera)
    }

    /// The view keys of every tile with a camera, in reading order.
    public func tileKeys(layout: String) -> [TileKey] {
        leaves.compactMap { tileKey(at: $0.path, layout: layout) }
    }
}

extension TileViews where Key == TileKey {
    /// Forgets the views of `layout` that no tile of `tree` has (a tile
    /// removed, or given another camera); other layouts' are kept.
    public mutating func prune(layout: String, tree: LayoutNode) {
        let keep = Set(tree.tileKeys(layout: layout))
        removeAll { $0.layout == layout && !keep.contains($0) }
    }

    /// Before the tiles at `p` and `q` of `tree` swap cameras: each view
    /// moves with its camera to the other tile.
    public mutating func swapViews(layout: String, tree: LayoutNode, _ p: NodePath, _ q: NodePath) {
        guard p != q, let ip = tree[p]?.tileID, let iq = tree[q]?.tileID else { return }
        var moves: [(from: Key, to: Key)] = []
        if let cp = tree[p]?.camera {
            moves.append((TileKey(layout: layout, tile: ip, camera: cp), TileKey(layout: layout, tile: iq, camera: cp)))
        }
        if let cq = tree[q]?.camera {
            moves.append((TileKey(layout: layout, tile: iq, camera: cq), TileKey(layout: layout, tile: ip, camera: cq)))
        }
        move(moves)
    }

    /// The window's content took another name (an unsaved arrangement saved
    /// as a layout, a group edited into the window's own arrangement): its
    /// views go along, replacing any the new name had.
    public mutating func rename(layout old: String, to new: String) {
        guard old != new else { return }
        removeAll { $0.layout == new }
        move(order.filter { $0.layout == old }.map { ($0, TileKey(layout: new, tile: $0.tile, camera: $0.camera)) })
    }

    /// Whether a tile of `layout` has a Fill choice of its own.
    public func mixed(layout: String) -> Bool {
        entries.contains { $0.key.layout == layout && $0.value.fill != nil }
    }

    /// The views of a file written when they were kept per camera
    /// (windows.json "tileViews"): each camera's view goes to every tile of
    /// `tree` showing it, in reading order.
    public mutating func migrate(_ legacy: [LegacyTileView], layout: String, tree: LayoutNode) {
        var byCamera: [CameraRef: TileView] = [:]
        for e in legacy { byCamera[e.camera] = e.view }
        for key in tree.tileKeys(layout: layout) {
            if let v = byCamera[key.camera] { restore(v, for: key) }
        }
    }

    /// What a saved window keeps.
    public var savedEntries: [SavedTileView] { saved.map { SavedTileView(key: $0.key, view: $0.view) } }

    /// Read back from a saved window.
    public init(fill: Bool, savedEntries: [SavedTileView]) {
        self.init(fill: fill, saved: savedEntries.map { ($0.key, $0.view) })
    }
}

// MARK: Saved form

/// One tile's entry in a saved window (windows.json "tiles"): the content
/// it belongs to, the tile, the camera it shows, and its zoom or crop
/// position and own Fill choice. The view members are optional (what is
/// missing is the default); an entry without a layout, a tile or a camera
/// cannot be read and is skipped.
public struct SavedTileView: Codable, Equatable, Sendable {
    public var key: TileKey
    public var view: TileView

    public init(key: TileKey, view: TileView) {
        self.key = key
        self.view = view
    }

    private enum CodingKeys: String, CodingKey { case layout, tile, camera }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let layout = try c.decode(String.self, forKey: .layout)
        let tile = try c.decode(String.self, forKey: .tile)
        guard LayoutNode.validTileID(tile) else {
            throw DecodingError.dataCorruptedError(forKey: .tile, in: c, debugDescription: "no tile")
        }
        key = TileKey(layout: layout, tile: tile, camera: try c.decode(CameraRef.self, forKey: .camera))
        view = try SavedViewMembers.read(decoder)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(key.layout, forKey: .layout)
        try c.encode(key.tile, forKey: .tile)
        try c.encode(key.camera, forKey: .camera)
        try SavedViewMembers.write(view, encoder)
    }
}

/// One camera's entry as files kept them before views were per tile
/// (windows.json "tileViews"): read once, to give each tile showing the
/// camera its view (`TileViews.migrate`), and never written.
public struct LegacyTileView: Decodable, Equatable, Sendable {
    public var camera: CameraRef
    public var view: TileView

    public init(camera: CameraRef, view: TileView) {
        self.camera = camera
        self.view = view
    }

    private enum CodingKeys: String, CodingKey { case camera }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(CameraRef.self, forKey: .camera)
        view = try SavedViewMembers.read(decoder)
    }
}

/// The view members of a saved entry: scale, x, y (left out while 1x and
/// centered) and fill; a member of the wrong type is the default.
private enum SavedViewMembers {
    private enum Keys: String, CodingKey { case scale, x, y, fill }

    static func read(_ decoder: any Decoder) throws -> TileView {
        let c = try decoder.container(keyedBy: Keys.self)
        func number(_ key: Keys, _ fallback: Double) -> Double {
            ((try? c.decodeIfPresent(Double.self, forKey: key)) ?? nil) ?? fallback
        }
        let zoom = TileZoom(scale: number(.scale, 1), center: CGPoint(x: number(.x, 0.5), y: number(.y, 0.5)))
        return TileView(zoom: zoom.sanitized, fill: (try? c.decodeIfPresent(Bool.self, forKey: .fill)) ?? nil)
    }

    static func write(_ view: TileView, _ encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        if !view.zoom.isDefault {
            try c.encode(view.zoom.scale, forKey: .scale)
            try c.encode(Double(view.zoom.center.x), forKey: .x)
            try c.encode(Double(view.zoom.center.y), forKey: .y)
        }
        try c.encodeIfPresent(view.fill, forKey: .fill)
    }
}

/// Decodes what it can: an element that fails is nil instead of failing the
/// whole array.
struct Lossy<T: Decodable>: Decodable {
    let value: T?
    init(from decoder: any Decoder) throws { value = try? T(from: decoder) }
}
