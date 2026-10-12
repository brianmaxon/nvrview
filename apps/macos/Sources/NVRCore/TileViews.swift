import CoreGraphics
import Foundation

/// How one camera's picture is shown in one window (docs/UX.md "Fill
/// Tiles"): its digital zoom, or at 1x where its filled crop sits, and its
/// own Fill choice when it differs from the window's.
public struct TileView: Equatable, Sendable {
    public var zoom: TileZoom
    /// The camera's own Fill choice; nil follows the window's.
    public var fill: Bool?

    public init(zoom: TileZoom = .none, fill: Bool? = nil) {
        self.zoom = zoom
        self.fill = fill
    }
}

/// A window's picture settings per camera (docs/UX.md "Fill Tiles"): the
/// window's Fill Tiles switch and, per camera, the zoom or crop position
/// and a Fill choice of its own. Keyed by camera, so tiles of the same
/// camera in one window share it, and it follows the camera through swaps
/// and layout changes; another window has its own. Positions are fractions
/// of the picture, so the sub-stream and the main stream show the same
/// part.
///
/// The rule for Fill: a tile fills if its camera has its own choice, else
/// as the window says. A choice equal to the window's is not kept, and
/// setting the window's switch clears every camera's choice (all tiles
/// follow it). At most `limit` cameras are kept; the one changed longest
/// ago goes first. Pure; the vectors in `fixtures/synthetic/tile-views.json`
/// are shared with the Windows app's TileViews (NvrView.Tiles).
public struct TileViews<Key: Hashable & Sendable>: Equatable, Sendable {
    /// The most cameras one window keeps.
    public static var limit: Int { 256 }

    /// The window's Fill Tiles switch: what tiles without a choice of
    /// their own do.
    public private(set) var fill: Bool
    public private(set) var entries: [Key: TileView] = [:]
    /// The cameras kept, the one changed longest ago first.
    public private(set) var order: [Key] = []

    public init(fill: Bool = false) { self.fill = fill }

    /// The camera's zoom (1x and centered when nothing is kept).
    public func zoom(_ key: Key) -> TileZoom { entries[key]?.zoom ?? .none }

    /// Whether the camera's tiles fill: its own choice, else the window's.
    public func fills(_ key: Key) -> Bool { entries[key]?.fill ?? fill }

    /// The camera's own Fill choice (nil: it follows the window).
    public func fillChoice(_ key: Key) -> Bool? { entries[key]?.fill }

    /// Some camera has a Fill choice of its own (the window's switch then
    /// shows mixed where the platform can).
    public var mixed: Bool { entries.values.contains { $0.fill != nil } }

    public var count: Int { order.count }

    public mutating func setZoom(_ zoom: TileZoom, for key: Key) {
        var v = entries[key] ?? TileView()
        v.zoom = zoom
        put(v, for: key)
    }

    /// The camera's tiles fill, or fit (a choice equal to the window's is
    /// not kept).
    public mutating func setFill(_ fills: Bool, for key: Key) {
        var v = entries[key] ?? TileView()
        v.fill = fills == fill ? nil : fills
        put(v, for: key)
    }

    /// The window's switch: every tile follows it (the cameras' own
    /// choices are cleared; their positions stay).
    public mutating func setWindowFill(_ fills: Bool) {
        fill = fills
        for key in order {
            guard var v = entries[key], v.fill != nil else { continue }
            v.fill = nil
            entries[key] = v.zoom.isDefault ? nil : v
        }
        order.removeAll { entries[$0] == nil }
    }

    /// Forgets the camera (1x, centered, following the window).
    public mutating func remove(_ key: Key) {
        entries[key] = nil
        order.removeAll { $0 == key }
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

    /// The kept cameras, the one changed longest ago first (what is saved).
    public var saved: [(key: Key, view: TileView)] { order.compactMap { k in entries[k].map { (k, $0) } } }

    /// Read back from `saved` entries (oldest first): the values clamped,
    /// entries with nothing to keep dropped, a camera listed twice taken
    /// from its later entry, and at most `limit` kept (the newest).
    public init(fill: Bool, saved: [(key: Key, view: TileView)]) {
        self.fill = fill
        for (key, view) in saved {
            var v = view
            v.zoom = v.zoom.sanitized
            if v.fill == fill { v.fill = nil }
            put(v, for: key)
        }
    }
}

/// One camera's entry in a saved window (windows.json "tileViews"): the
/// camera, and its zoom or crop position and own Fill choice. The members
/// are optional; what is missing is the default, and an entry that cannot
/// be read is skipped.
public struct SavedTileView: Codable, Equatable, Sendable {
    public var camera: CameraRef
    public var view: TileView

    public init(camera: CameraRef, view: TileView) {
        self.camera = camera
        self.view = view
    }

    private enum CodingKeys: String, CodingKey { case camera, scale, x, y, fill }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(CameraRef.self, forKey: .camera)
        func number(_ key: CodingKeys, _ fallback: Double) -> Double {
            ((try? c.decodeIfPresent(Double.self, forKey: key)) ?? nil) ?? fallback
        }
        let zoom = TileZoom(scale: number(.scale, 1), center: CGPoint(x: number(.x, 0.5), y: number(.y, 0.5)))
        view = TileView(zoom: zoom.sanitized, fill: (try? c.decodeIfPresent(Bool.self, forKey: .fill)) ?? nil)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(camera, forKey: .camera)
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

extension TileViews where Key == CameraRef {
    /// What a saved window keeps.
    public var savedEntries: [SavedTileView] { saved.map { SavedTileView(camera: $0.key, view: $0.view) } }

    /// Read back from a saved window.
    public init(fill: Bool, savedEntries: [SavedTileView]) {
        self.init(fill: fill, saved: savedEntries.map { ($0.camera, $0.view) })
    }
}
