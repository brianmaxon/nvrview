import Foundation

// Per-tile stream quality (docs/UX.md "Stream quality per tile"): the choice
// belongs to a layout tile and is saved as the leaf's optional `quality`
// member; the tiers it maps to are decided per camera by `TileLeases`.

/// What a tile asks of its camera's stream.
public enum TileQuality: String, Sendable, CaseIterable {
    /// The low stream in the grid, the main stream when focused or zoomed
    /// past `ZoomHysteresis.up`. Stored as no member at all.
    case auto
    /// The main stream (a second lens's `extern`), also in the grid.
    case clear
    /// The low stream, also when focused.
    case fluent
    /// The middle stream (`extern` of a single-lens camera).
    case balanced

    /// The words the menu and the hover header use.
    public var title: String {
        switch self {
        case .auto: "Auto"
        case .clear: "Clear"
        case .fluent: "Fluent"
        case .balanced: "Balanced"
        }
    }
}

/// How sharp a stream is, whatever its tier is called for a camera. Higher
/// wins when tiles share a camera.
public enum StreamLevel: Int, Sendable, Comparable {
    /// `sub` (a second lens's `mobile`, or `extern` where it has no `mobile`).
    case low = 0
    /// A single-lens camera's `extern`.
    case mid = 1
    /// `main` (a second lens's `extern`).
    case high = 2

    public static func < (a: StreamLevel, b: StreamLevel) -> Bool { a.rawValue < b.rawValue }
}

/// Auto's switch to the high stream while a tile is zoomed in: up at 2x,
/// back down only below 1.6x, so a zoom hovering near the threshold does not
/// flap between streams.
public enum ZoomHysteresis {
    public static let up = 2.0
    public static let down = 1.6

    /// Whether the tile should be on the high stream at `scale`, given
    /// whether it was.
    public static func next(high: Bool, scale: Double) -> Bool {
        high ? scale >= down : scale >= up
    }
}

extension LayoutNode {
    private static let qualityKey = "quality"

    /// The quality of the tile at this leaf (`auto` for a split, no member,
    /// or a value a newer app wrote that this one does not know).
    public var quality: TileQuality {
        guard case .leaf(_, let extras) = self, case .string(let s)? = extras.node[Self.qualityKey] else { return .auto }
        return TileQuality(rawValue: s) ?? .auto
    }

    /// The tree with the leaf at `path` set to `quality` (Auto removes the
    /// member). Nil if `path` is not a leaf.
    public func settingQuality(_ quality: TileQuality, at path: NodePath) -> LayoutNode? {
        guard case .leaf(let camera, var extras) = self[path] else { return nil }
        if quality == .auto {
            extras.node[Self.qualityKey] = nil
        } else {
            extras.node[Self.qualityKey] = .string(quality.rawValue)
        }
        return replacing(at: path, with: .leaf(camera, extras: extras))
    }
}
