import Foundation

// Split-pane layouts (docs/UX.md "Layouts", docs/FFI-ABI3.md section 4): a
// layout is a binary tree whose leaves are tiles, each showing one camera or
// nothing, and whose inner nodes split their rectangle in two, side by side
// (`row`) or stacked (`col`), at a ratio. Pure values: every operation
// returns a new tree, so the designer can undo by keeping the old one and
// tests check invariants without a window.

/// A camera as a layout names it: the core's stable device key (not the
/// per-run device ID), a channel, and the lens (0 for the first; 1 for the
/// second lens of a multi-lens camera, which the device serves on the same
/// channel). Stored with `lens` only when it is not 0, so single-lens
/// cameras read and write as before.
public struct CameraRef: Hashable, Sendable, Codable, CustomStringConvertible {
    public var device: String
    public var channel: Int
    public var lens: Int

    public static let maxLens = 1

    public init(device: String, channel: Int, lens: Int = 0) {
        self.device = device
        self.channel = channel
        self.lens = lens
    }

    /// The same camera under another device key.
    public func rekeyed(to device: String) -> CameraRef { CameraRef(device: device, channel: channel, lens: lens) }

    /// The first lens of the same channel.
    public var firstLens: CameraRef { CameraRef(device: device, channel: channel) }

    public var description: String { lens == 0 ? "\(device)/\(channel)" : "\(device)/\(channel)/lens\(lens)" }

    private enum CodingKeys: String, CodingKey { case device, channel, lens }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        device = try c.decode(String.self, forKey: .device)
        channel = try c.decode(Int.self, forKey: .channel)
        lens = try c.decodeIfPresent(Int.self, forKey: .lens) ?? 0
        guard (0...Self.maxLens).contains(lens) else {
            throw DecodingError.dataCorruptedError(forKey: .lens, in: c, debugDescription: "lens out of range")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(device, forKey: .device)
        try c.encode(channel, forKey: .channel)
        if lens != 0 { try c.encode(lens, forKey: .lens) }
    }
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

/// Members of a stored node this app does not know, kept so that saving a
/// layout writes them back (a newer app's additions survive this app's
/// edits). `node` belongs to the tile or split itself and stays with it;
/// `camera` belongs to the camera object of a leaf and goes away when the
/// tile is given another camera.
public struct NodeExtras: Sendable, Hashable {
    public var node: [String: JSONValue]
    public var camera: [String: JSONValue]

    public init(node: [String: JSONValue] = [:], camera: [String: JSONValue] = [:]) {
        self.node = node
        self.camera = camera
    }

    public static let none = NodeExtras()

    /// The same, without what belonged to the camera.
    var withoutCamera: NodeExtras { NodeExtras(node: node) }
}

public indirect enum LayoutNode: Sendable, Equatable {
    case leaf(CameraRef?, extras: NodeExtras = .none)
    case split(SplitAxis, ratio: Double, a: LayoutNode, b: LayoutNode, extras: NodeExtras = .none)

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

    public var camera: CameraRef? { if case .leaf(let c, _) = self { c } else { nil } }

    /// The members of this node the app does not know.
    public var extras: NodeExtras {
        switch self {
        case .leaf(_, let e), .split(_, _, _, _, let e): e
        }
    }

    public subscript(path: NodePath) -> LayoutNode? {
        var node = self
        for side in path {
            guard case .split(_, _, let a, let b, _) = node else { return nil }
            node = side == .a ? a : b
        }
        return node
    }

    /// Every leaf in reading order (`a` before `b`).
    public var leaves: [(path: NodePath, camera: CameraRef?)] {
        var out: [(NodePath, CameraRef?)] = []
        func walk(_ n: LayoutNode, _ path: NodePath) {
            switch n {
            case .leaf(let c, _): out.append((path, c))
            case .split(_, _, let a, let b, _):
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
        case .split(_, _, let a, let b, _): a.leafCount + b.leafCount
        }
    }

    public var depth: Int {
        switch self {
        case .leaf: 1
        case .split(_, _, let a, let b, _): 1 + max(a.depth, b.depth)
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
        guard case .split(let axis, let ratio, let a, let b, let extras) = self else { return nil }
        let rest = Array(path.dropFirst())
        if first == .a {
            guard let na = a.replacing(at: rest, with: node) else { return nil }
            return .split(axis, ratio: ratio, a: na, b: b, extras: extras)
        } else {
            guard let nb = b.replacing(at: rest, with: node) else { return nil }
            return .split(axis, ratio: ratio, a: a, b: nb, extras: extras)
        }
    }

    /// Splits the leaf at `path` in two: its camera stays in `a`, `b` is a
    /// new empty tile (or `camera`). Returns the new tree and the new tile's
    /// path; nil if `path` is not a leaf or the tree would grow too large.
    public func splitting(at path: NodePath, axis: SplitAxis, camera: CameraRef? = nil, ratio: Double = 0.5)
        -> (tree: LayoutNode, newLeaf: NodePath)?
    {
        guard let existing = self[path], existing.isLeaf, leafCount < Self.maxLeaves,
            path.count + 2 <= Self.maxDepth
        else { return nil }
        // The old tile (with its members) moves into `a`.
        let node = LayoutNode.split(axis, ratio: Self.clamp(ratio), a: existing, b: .leaf(camera))
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
    /// Each camera takes its own members along; the tiles keep theirs.
    public func swapping(_ p: NodePath, _ q: NodePath) -> LayoutNode? {
        guard case .leaf(let cp, let ep) = self[p], case .leaf(let cq, let eq) = self[q] else { return nil }
        if p == q { return self }
        return replacing(at: p, with: .leaf(cq, extras: NodeExtras(node: ep.node, camera: eq.camera)))?
            .replacing(at: q, with: .leaf(cp, extras: NodeExtras(node: eq.node, camera: ep.camera)))
    }

    /// Shows `camera` (or nothing) in the leaf at `path`. The tile keeps its
    /// own members; the previous camera's go with it.
    public func assigning(_ camera: CameraRef?, at path: NodePath) -> LayoutNode? {
        guard case .leaf(let old, let extras) = self[path] else { return nil }
        if old == camera { return self }
        return replacing(at: path, with: .leaf(camera, extras: extras.withoutCamera))
    }

    /// Sets the ratio of the split at `path`, kept within `minRatio...maxRatio`.
    public func settingRatio(_ ratio: Double, at path: NodePath) -> LayoutNode? {
        guard case .split(let axis, _, let a, let b, let extras) = self[path] else { return nil }
        return replacing(at: path, with: .split(axis, ratio: Self.clamp(ratio), a: a, b: b, extras: extras))
    }

    /// Replaces every camera of `device` with nothing (a removed device).
    public func clearing(device: String) -> LayoutNode {
        switch self {
        case .leaf(let c, let extras): c?.device == device ? .leaf(nil, extras: extras.withoutCamera) : self
        case .split(let axis, let ratio, let a, let b, let extras):
            .split(axis, ratio: ratio, a: a.clearing(device: device), b: b.clearing(device: device), extras: extras)
        }
    }

    /// Renames a device key in every leaf (the core gave the device a key).
    public func rekeying(device old: String, to new: String) -> LayoutNode {
        switch self {
        case .leaf(let c, let extras):
            c?.device == old ? .leaf(c!.rekeyed(to: new), extras: extras) : self
        case .split(let axis, let ratio, let a, let b, let extras):
            .split(
                axis, ratio: ratio, a: a.rekeying(device: old, to: new), b: b.rekeying(device: old, to: new),
                extras: extras)
        }
    }

    /// The same tree with every ratio clamped into range (for trees read
    /// from elsewhere).
    public var normalized: LayoutNode {
        switch self {
        case .leaf: self
        case .split(let axis, let ratio, let a, let b, let extras):
            .split(
                axis, ratio: Self.clamp(ratio.isFinite ? ratio : 0.5), a: a.normalized, b: b.normalized,
                extras: extras)
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
    private static let splitKeys: Set<String> = ["split", "ratio", "a", "b"]
    private static let leafKeys: Set<String> = ["camera"]
    private static let cameraKeys: Set<String> = ["device", "channel", "lens"]

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: AnyKey.self)
        if let axis = try c.decodeIfPresent(SplitAxis.self, forKey: AnyKey("split")) {
            let ratioKey = AnyKey("ratio")
            let ratio = try c.decode(Double.self, forKey: ratioKey)
            guard ratio > 0, ratio < 1 else {
                throw DecodingError.dataCorruptedError(forKey: ratioKey, in: c, debugDescription: "ratio out of range")
            }
            self = .split(
                axis, ratio: ratio, a: try c.decode(LayoutNode.self, forKey: AnyKey("a")),
                b: try c.decode(LayoutNode.self, forKey: AnyKey("b")),
                extras: NodeExtras(node: try c.unknownMembers(except: Self.splitKeys)))
            return
        }
        let cameraKey = AnyKey("camera")
        guard c.contains(cameraKey) else {
            throw DecodingError.keyNotFound(cameraKey, .init(codingPath: c.codingPath, debugDescription: "no camera or split"))
        }
        let node = try c.unknownMembers(except: Self.leafKeys)
        if try c.decodeNil(forKey: cameraKey) {
            self = .leaf(nil, extras: NodeExtras(node: node))
            return
        }
        let cam = try c.nestedContainer(keyedBy: AnyKey.self, forKey: cameraKey)
        let ref = try c.decode(CameraRef.self, forKey: cameraKey)
        self = .leaf(ref, extras: NodeExtras(node: node, camera: try cam.unknownMembers(except: Self.cameraKeys)))
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: AnyKey.self)
        switch self {
        case .leaf(let camera, let extras):
            try c.encodeUnknown(extras.node, except: Self.leafKeys)
            if let camera {
                var cam = c.nestedContainer(keyedBy: AnyKey.self, forKey: AnyKey("camera"))
                try cam.encodeUnknown(extras.camera, except: Self.cameraKeys)
                try cam.encode(camera.device, forKey: AnyKey("device"))
                try cam.encode(camera.channel, forKey: AnyKey("channel"))
                if camera.lens != 0 { try cam.encode(camera.lens, forKey: AnyKey("lens")) }
            } else {
                try c.encodeNil(forKey: AnyKey("camera"))
            }
        case .split(let axis, let ratio, let a, let b, let extras):
            try c.encodeUnknown(extras.node, except: Self.splitKeys)
            try c.encode(axis, forKey: AnyKey("split"))
            try c.encode(ratio, forKey: AnyKey("ratio"))
            try c.encode(a, forKey: AnyKey("a"))
            try c.encode(b, forKey: AnyKey("b"))
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
    /// Members of the layout this app does not know, written back on save.
    public var extras: [String: JSONValue]

    public init(
        id: String = SavedLayout.newID(), name: String, version: Int = 0, root: LayoutNode,
        extras: [String: JSONValue] = [:]
    ) {
        self.id = id
        self.name = name
        self.version = version
        self.root = root
        self.extras = extras
    }

    private static let known: Set<String> = ["id", "name", "version", "root"]

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: AnyKey.self)
        id = try c.decode(String.self, forKey: AnyKey("id"))
        name = try c.decode(String.self, forKey: AnyKey("name"))
        version = try c.decodeIfPresent(Int.self, forKey: AnyKey("version")) ?? 0
        root = try c.decode(LayoutNode.self, forKey: AnyKey("root")).normalized
        extras = try c.unknownMembers(except: Self.known)
        guard root.depth <= LayoutNode.maxDepth, root.leafCount <= LayoutNode.maxLeaves else {
            throw DecodingError.dataCorruptedError(forKey: AnyKey("root"), in: c, debugDescription: "layout too large")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: AnyKey.self)
        try c.encodeUnknown(extras, except: Self.known)
        try c.encode(id, forKey: AnyKey("id"))
        try c.encode(name, forKey: AnyKey("name"))
        try c.encode(version, forKey: AnyKey("version"))
        try c.encode(root, forKey: AnyKey("root"))
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
