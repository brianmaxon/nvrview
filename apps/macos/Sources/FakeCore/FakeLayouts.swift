import Foundation
import NVRCore
import Synchronization

/// The layouts store the real core keeps (docs/FFI-ABI3.md section 4),
/// modeled for the fake: it validates each layout, versions it, keeps at most
/// `limit`, and optionally writes them to a file atomically.
///
/// What it checks, as docs/FFI.md "Layouts" says the core does:
/// - the object has a non-empty string `id` and a string `name` (each at
///   most 256 bytes) and a `root` node; the whole layout at most 64 KiB;
/// - a node is either a leaf `{"camera": null}` or
///   `{"camera": {"device": "<key, 1...256 bytes>", "channel": 0...255}}`,
///   or a split `{"split": "row"|"col", "ratio": 0.02...0.98, "a": node,
///   "b": node}`;
/// - at most `maxSplitDepth` splits deep and `maxLeaves` tiles.
///
/// Versioning: the stored `version` is 1 for a new id and one more than the
/// stored one on every replace, whatever the caller sent. Unknown fields are
/// kept as they are, so a newer app's additions survive an older app's edit.
///
/// `nv_layouts_get` returns `{"revision":N,"layouts":[...]}` in insertion
/// order; `revision` grows with every change.
final class FakeLayouts: Sendable {
    static let maxSplitDepth = 16
    static let maxLeaves = 64
    static let maxBytes = 64 * 1024

    private struct Stored: Sendable {
        var ids: [String] = []
        var revision = 0
        /// Canonical JSON (sorted keys) per id, version included.
        var json: [String: String] = [:]
    }

    private let stored: Mutex<Stored>
    private let file: URL?
    let limit: Int

    init(file: URL?, limit: Int) {
        self.file = file
        self.limit = limit
        var s = Stored()
        if let file, let data = try? Data(contentsOf: file),
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let list = object["layouts"] as? [[String: Any]]
        {
            s.revision = (object["revision"] as? NSNumber)?.intValue ?? 0
            for layout in list where (try? Self.validate(layout)) != nil {
                guard let id = layout["id"] as? String, s.json[id] == nil, let text = Self.canonical(layout) else {
                    continue
                }
                s.ids.append(id)
                s.json[id] = text
            }
        }
        stored = Mutex(s)
    }

    func all() -> String { stored.withLock { Self.document($0) } }

    private static func document(_ s: Stored) -> String {
        #"{"revision":\#(s.revision),"layouts":["# + s.ids.compactMap { s.json[$0] }.joined(separator: ",") + "]}"
    }

    /// Returns the new revision.
    func put(_ json: String) throws(CoreStatus) -> Int {
        guard json.utf8.count <= Self.maxBytes, let data = json.data(using: .utf8),
            var layout = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { throw .invalidArgument }
        try Self.validate(layout)
        let id = layout["id"] as! String
        let (snapshot, revision): (String, Int) = try stored.withLock { s throws(CoreStatus) in
            let previous = s.json[id].flatMap { Self.version(of: $0) } ?? 0
            if s.json[id] == nil && s.ids.count >= limit { throw .limit }
            layout["version"] = previous + 1
            guard let text = Self.canonical(layout) else { throw .invalidArgument }
            if s.json[id] == nil { s.ids.append(id) }
            s.json[id] = text
            s.revision += 1
            return (Self.document(s), s.revision)
        }
        write(snapshot)
        return revision
    }

    /// Returns the new revision.
    func delete(_ id: String) throws(CoreStatus) -> Int {
        let (snapshot, revision): (String, Int) = try stored.withLock { s throws(CoreStatus) in
            guard s.json.removeValue(forKey: id) != nil else { throw .unknownID }
            s.ids.removeAll { $0 == id }
            s.revision += 1
            return (Self.document(s), s.revision)
        }
        write(snapshot)
        return revision
    }

    private func write(_ text: String) {
        guard let file else { return }
        try? FileManager.default.createDirectory(
            at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? Data(text.utf8).write(to: file, options: .atomic)
    }

    // MARK: Validation

    static func validate(_ layout: [String: Any]) throws(CoreStatus) {
        guard let id = layout["id"] as? String, !id.isEmpty, id.utf8.count <= 256,
            let name = layout["name"] as? String, name.utf8.count <= 256, let root = layout["root"]
        else { throw .invalidArgument }
        var leaves = 0
        try node(root, depth: 0, leaves: &leaves)
    }

    private static func node(_ value: Any, depth: Int, leaves: inout Int) throws(CoreStatus) {
        guard let n = value as? [String: Any] else { throw .invalidArgument }
        if let split = n["split"] {
            guard depth < maxSplitDepth, let axis = split as? String, axis == "row" || axis == "col",
                let ratio = (n["ratio"] as? NSNumber)?.doubleValue, ratio >= 0.02, ratio <= 0.98,
                let a = n["a"], let b = n["b"]
            else { throw .invalidArgument }
            try node(a, depth: depth + 1, leaves: &leaves)
            try node(b, depth: depth + 1, leaves: &leaves)
            return
        }
        guard n.keys.contains("camera") else { throw .invalidArgument }
        leaves += 1
        guard leaves <= maxLeaves else { throw .invalidArgument }
        if n["camera"] is NSNull { return }
        guard let camera = n["camera"] as? [String: Any], let device = camera["device"] as? String,
            !device.isEmpty, device.utf8.count <= 256, let channel = camera["channel"] as? NSNumber,
            CFGetTypeID(channel) == CFNumberGetTypeID(), CFNumberIsFloatType(channel) == false, (0...255).contains(channel.intValue)
        else { throw .invalidArgument }
    }

    private static func canonical(_ layout: [String: Any]) -> String? {
        guard let data = try? JSONSerialization.data(withJSONObject: layout, options: [.sortedKeys]) else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    private static func version(of json: String) -> Int? {
        guard let data = json.data(using: .utf8),
            let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { return nil }
        return (object["version"] as? NSNumber)?.intValue
    }
}
