import Foundation

// Split-pane layouts (docs/UX.md "Layouts", docs/FFI-ABI3.md section 4): a
// layout is a binary tree whose leaves are tiles, each showing one camera or
// nothing, and whose inner nodes split their rectangle in two, side by side
// (`row`) or stacked (`col`), at a ratio. Pure values: every operation
// returns a new tree, so the designer can undo by keeping the old one and
// tests check invariants without a window.

/// A camera as a layout names it: the core's stable device key (not the
/// per-run device ID) and a channel.
public struct CameraRef: Hashable, Sendable, Codable, CustomStringConvertible {
    public var device: String
    public var channel: Int

    public init(device: String, channel: Int) {
        self.device = device
        self.channel = channel
    }

    public var description: String { "\(device)/\(channel)" }
}

/// How a split divides its rectangle.
public enum SplitAxis: String, Sendable, Codable, CaseIterable {
    /// Side by side: `a` on the left, `b` on the right.
    case row
    /// Stacked: `a` on top, `b` below.
    case col
}

/// One step from a split to a child.
public enum Side: Sendable, Hashable {
    case a, b

    public var other: Side { self == .a ? .b : .a }
}

/// Where a node sits: the sides taken from the root (empty for the root).
public typealias NodePath = [Side]

public indirect enum LayoutNode: Sendable, Equatable {
    case leaf(CameraRef?)
    case split(SplitAxis, ratio: Double, a: LayoutNode, b: LayoutNode)

    /// The narrowest a pane may be dragged to, as a share of its split.
    public static let minRatio = 0.1
    public static let maxRatio = 0.9
    /// Deeper trees are refused (the core refuses them too).
    /// At most 16 splits deep (17 levels), as the core stores them.
    public static let maxDepth = 17
    public static let maxLeaves = 64

    public static let empty = LayoutNode.leaf(nil)

    // MARK: Reading

    public var isLeaf: Bool { if case .leaf = self { true } else { false } }

    public var camera: CameraRef? { if case .leaf(let c) = self { c } else { nil } }

    public subscript(path: NodePath) -> LayoutNode? {
        var node = self
        for side in path {
            guard case .split(_, _, let a, let b) = node else { return nil }
            node = side == .a ? a : b
        }
        return node
    }

    /// Every leaf in reading order (`a` before `b`).
    public var leaves: [(path: NodePath, camera: CameraRef?)] {
        var out: [(NodePath, CameraRef?)] = []
        func walk(_ n: LayoutNode, _ path: NodePath) {
            switch n {
            case .leaf(let c): out.append((path, c))
            case .split(_, _, let a, let b):
                walk(a, path + [.a])
                walk(b, path + [.b])
            }
        }
        walk(self, [])
        return out
    }

    public var leafCount: Int {
        switch self {
        case .leaf: 1
        case .split(_, _, let a, let b): a.leafCount + b.leafCount
        }
    }

    public var depth: Int {
        switch self {
        case .leaf: 1
        case .split(_, _, let a, let b): 1 + max(a.depth, b.depth)
        }
    }

    /// The cameras shown, each once however many tiles show it.
    public var cameras: Set<CameraRef> { Set(leaves.compactMap(\.camera)) }

    /// Every camera in reading order, with repeats.
    public var cameraList: [CameraRef?] { leaves.map(\.camera) }

    // MARK: Editing

    /// The tree with the node at `path` replaced; nil if there is none.
    public func replacing(at path: NodePath, with node: LayoutNode) -> LayoutNode? {
        guard let first = path.first else { return node }
        guard case .split(let axis, let ratio, let a, let b) = self else { return nil }
        let rest = Array(path.dropFirst())
        if first == .a {
            guard let na = a.replacing(at: rest, with: node) else { return nil }
            return .split(axis, ratio: ratio, a: na, b: b)
        } else {
            guard let nb = b.replacing(at: rest, with: node) else { return nil }
            return .split(axis, ratio: ratio, a: a, b: nb)
        }
    }

    /// Splits the leaf at `path` in two: its camera stays in `a`, `b` is a
    /// new empty tile (or `camera`). Returns the new tree and the new tile's
    /// path; nil if `path` is not a leaf or the tree would grow too large.
    public func splitting(at path: NodePath, axis: SplitAxis, camera: CameraRef? = nil, ratio: Double = 0.5)
        -> (tree: LayoutNode, newLeaf: NodePath)?
    {
        guard case .leaf(let existing) = self[path], leafCount < Self.maxLeaves,
            path.count + 2 <= Self.maxDepth
        else { return nil }
        let node = LayoutNode.split(
            axis, ratio: Self.clamp(ratio), a: .leaf(existing), b: .leaf(camera))
        guard let tree = replacing(at: path, with: node) else { return nil }
        return (tree, path + [.b])
    }

    /// Removes the leaf at `path`; its sibling takes the parent's whole
    /// rectangle. Removing the only tile leaves one empty tile. Returns the
    /// new tree and the path of the node that took the space.
    public func removing(at path: NodePath) -> (tree: LayoutNode, survivor: NodePath)? {
        guard self[path]?.isLeaf == true else { return nil }
        guard let last = path.last else { return (.empty, []) }
        let parent = Array(path.dropLast())
        guard let sibling = self[parent + [last.other]], let tree = replacing(at: parent, with: sibling) else {
            return nil
        }
        return (tree, parent)
    }

    /// Swaps the cameras of two leaves; the tree's shape stays as it is.
    public func swapping(_ p: NodePath, _ q: NodePath) -> LayoutNode? {
        guard case .leaf(let cp) = self[p], case .leaf(let cq) = self[q] else { return nil }
        if p == q { return self }
        return replacing(at: p, with: .leaf(cq))?.replacing(at: q, with: .leaf(cp))
    }

    /// Shows `camera` (or nothing) in the leaf at `path`.
    public func assigning(_ camera: CameraRef?, at path: NodePath) -> LayoutNode? {
        guard self[path]?.isLeaf == true else { return nil }
        return replacing(at: path, with: .leaf(camera))
    }

    /// Sets the ratio of the split at `path`, kept within `minRatio...maxRatio`.
    public func settingRatio(_ ratio: Double, at path: NodePath) -> LayoutNode? {
        guard case .split(let axis, _, let a, let b) = self[path] else { return nil }
        return replacing(at: path, with: .split(axis, ratio: Self.clamp(ratio), a: a, b: b))
    }

    /// Replaces every camera of `device` with nothing (a removed device).
    public func clearing(device: String) -> LayoutNode {
        switch self {
        case .leaf(let c): c?.device == device ? .empty : self
        case .split(let axis, let ratio, let a, let b):
            .split(axis, ratio: ratio, a: a.clearing(device: device), b: b.clearing(device: device))
        }
    }

    /// Renames a device key in every leaf (the core gave the device a key).
    public func rekeying(device old: String, to new: String) -> LayoutNode {
        switch self {
        case .leaf(let c):
            c?.device == old ? .leaf(CameraRef(device: new, channel: c!.channel)) : self
        case .split(let axis, let ratio, let a, let b):
            .split(axis, ratio: ratio, a: a.rekeying(device: old, to: new), b: b.rekeying(device: old, to: new))
        }
    }

    /// The same tree with every ratio clamped into range (for trees read
    /// from elsewhere).
    public var normalized: LayoutNode {
        switch self {
        case .leaf: self
        case .split(let axis, let ratio, let a, let b):
            .split(axis, ratio: Self.clamp(ratio.isFinite ? ratio : 0.5), a: a.normalized, b: b.normalized)
        }
    }

    public static func clamp(_ ratio: Double) -> Double { min(max(ratio, minRatio), maxRatio) }

    // MARK: Building

    /// `nodes` in a line along `axis`, each the same size.
    public static func chain(_ axis: SplitAxis, _ nodes: [LayoutNode]) -> LayoutNode {
        guard let first = nodes.first else { return .empty }
        if nodes.count == 1 { return first }
        return .split(axis, ratio: 1 / Double(nodes.count), a: first, b: chain(axis, Array(nodes.dropFirst())))
    }

    /// A grid of equal tiles for `cameras` (rows of about sqrt(n) tiles, the
    /// last row possibly shorter, each row as wide as the others).
    public static func balanced(_ cameras: [CameraRef?]) -> LayoutNode {
        let list = Array(cameras.prefix(maxLeaves))
        guard !list.isEmpty else { return .empty }
        let columns = Int(Double(list.count).squareRoot().rounded(.up))
        var rows: [LayoutNode] = []
        var i = 0
        while i < list.count {
            let row = list[i..<min(i + columns, list.count)].map { LayoutNode.leaf($0) }
            rows.append(chain(.row, row))
            i += columns
        }
        return chain(.col, rows)
    }

    /// Starting points for the designer.
    public enum Preset: String, CaseIterable, Sendable {
        case single = "Single"
        case twoByTwo = "2 × 2"
        case onePlusThree = "1 + 3"
        case threeByThree = "3 × 3"

        /// The preset's shape, filled with `cameras` in order (empty tiles
        /// for the rest).
        public func tree(cameras: [CameraRef] = []) -> LayoutNode {
            func c(_ i: Int) -> LayoutNode { .leaf(i < cameras.count ? cameras[i] : nil) }
            switch self {
            case .single: return c(0)
            case .twoByTwo: return LayoutNode.balanced((0..<4).map { i in i < cameras.count ? cameras[i] : nil })
            case .onePlusThree:
                return .split(.row, ratio: 2.0 / 3, a: c(0), b: chain(.col, [c(1), c(2), c(3)]))
            case .threeByThree: return LayoutNode.balanced((0..<9).map { i in i < cameras.count ? cameras[i] : nil })
            }
        }
    }
}

// MARK: JSON (docs/FFI-ABI3.md section 4)

extension LayoutNode: Codable {
    private enum Keys: String, CodingKey { case camera, split, ratio, a, b }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        if let axis = try c.decodeIfPresent(SplitAxis.self, forKey: .split) {
            let ratio = try c.decode(Double.self, forKey: .ratio)
            guard ratio > 0, ratio < 1 else {
                throw DecodingError.dataCorruptedError(forKey: .ratio, in: c, debugDescription: "ratio out of range")
            }
            self = .split(axis, ratio: ratio, a: try c.decode(LayoutNode.self, forKey: .a), b: try c.decode(LayoutNode.self, forKey: .b))
            return
        }
        guard c.contains(.camera) else {
            throw DecodingError.keyNotFound(Keys.camera, .init(codingPath: c.codingPath, debugDescription: "no camera or split"))
        }
        self = .leaf(try c.decodeIfPresent(CameraRef.self, forKey: .camera))
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .leaf(let camera):
            if let camera { try c.encode(camera, forKey: .camera) } else { try c.encodeNil(forKey: .camera) }
        case .split(let axis, let ratio, let a, let b):
            try c.encode(axis, forKey: .split)
            try c.encode(ratio, forKey: .ratio)
            try c.encode(a, forKey: .a)
            try c.encode(b, forKey: .b)
        }
    }
}

/// A named layout as the core stores it.
public struct SavedLayout: Sendable, Equatable, Identifiable, Codable {
    public var id: String
    public var name: String
    /// Set by the core on every store (1, 2, ...); sent back unchanged.
    public var version: Int
    public var root: LayoutNode

    public init(id: String = SavedLayout.newID(), name: String, version: Int = 0, root: LayoutNode) {
        self.id = id
        self.name = name
        self.version = version
        self.root = root
    }

    private enum Keys: String, CodingKey { case id, name, version, root }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        version = try c.decodeIfPresent(Int.self, forKey: .version) ?? 0
        root = try c.decode(LayoutNode.self, forKey: .root).normalized
        guard root.depth <= LayoutNode.maxDepth, root.leafCount <= LayoutNode.maxLeaves else {
            throw DecodingError.dataCorruptedError(forKey: .root, in: c, debugDescription: "layout too large")
        }
    }

    public static func newID() -> String { "layout-" + UUID().uuidString.lowercased().prefix(8) }

    /// The JSON `nv_layout_put` takes.
    public var json: String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(self) else { return "{}" }
        return String(decoding: data, as: UTF8.self)
    }

    /// Reads what `nv_layouts_get` returns: `{"layouts":[...]}`, or a bare
    /// array. Entries that do not parse are skipped (a newer app may store
    /// shapes this one does not know).
    public static func list(fromJSON json: String) -> [SavedLayout] {
        struct Wrapped: Decodable { var layouts: [Lenient] }
        struct Lenient: Decodable {
            var layout: SavedLayout?
            init(from decoder: any Decoder) throws { layout = try? SavedLayout(from: decoder) }
        }
        let data = Data(json.utf8)
        if let w = try? JSONDecoder().decode(Wrapped.self, from: data) { return w.layouts.compactMap(\.layout) }
        if let a = try? JSONDecoder().decode([Lenient].self, from: data) { return a.compactMap(\.layout) }
        return []
    }
}
