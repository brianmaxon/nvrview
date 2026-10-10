import Foundation
import NVRCore
import Synchronization

/// The layouts store the real core keeps (docs/FFI-ABI3.md section 4),
/// modeled for the fake: it validates each layout, versions it, keeps at most
/// `limit`, and optionally writes them to a file atomically.
///
/// What it checks, and what the real core must check too:
/// - the object has a non-empty string `id` (at most 128 bytes), a string
///   `name`, and a `root` node;
/// - a node is either a leaf `{"camera": null}` or
///   `{"camera": {"device": "<non-empty key>", "channel": 0...255}}`, or a
///   split `{"split": "row"|"col", "ratio": r, "a": node, "b": node}` with
///   0 < r < 1;
/// - at most `maxDepth` levels and `maxLeaves` leaves.
///
/// Versioning: the stored `version` is 1 for a new id and one more than the
/// stored one on every replace, whatever the caller sent. Unknown fields are
/// kept as they are, so a newer app's additions survive an older app's edit.
///
/// `nv_layouts_get` returns `{"layouts":[...]}` in insertion order.
final class FakeLayouts: Sendable {
    static let maxDepth = 32
    static let maxLeaves = 64

    private struct Stored: Sendable {
        var ids: [String] = []
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

    func all() -> String {
        stored.withLock { s in
            #"{"layouts":["# + s.ids.compactMap { s.json[$0] }.joined(separator: ",") + "]}"
        }
    }

    func put(_ json: String) throws(CoreStatus) {
        guard let data = json.data(using: .utf8),
            var layout = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { throw .invalidArgument }
        try Self.validate(layout)
        let id = layout["id"] as! String
        let snapshot: String = try stored.withLock { s throws(CoreStatus) in
            let previous = s.json[id].flatMap { Self.version(of: $0) } ?? 0
            if s.json[id] == nil && s.ids.count >= limit { throw .limit }
            layout["version"] = previous + 1
            guard let text = Self.canonical(layout) else { throw .invalidArgument }
            if s.json[id] == nil { s.ids.append(id) }
            s.json[id] = text
            return #"{"layouts":["# + s.ids.compactMap { s.json[$0] }.joined(separator: ",") + "]}"
        }
        write(snapshot)
    }

    func delete(_ id: String) throws(CoreStatus) {
        let snapshot: String = try stored.withLock { s throws(CoreStatus) in
            guard s.json.removeValue(forKey: id) != nil else { throw .unknownID }
            s.ids.removeAll { $0 == id }
            return #"{"layouts":["# + s.ids.compactMap { s.json[$0] }.joined(separator: ",") + "]}"
        }
        write(snapshot)
    }

    private func write(_ text: String) {
        guard let file else { return }
        try? FileManager.default.createDirectory(
            at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? Data(text.utf8).write(to: file, options: .atomic)
    }

    // MARK: Validation

    static func validate(_ layout: [String: Any]) throws(CoreStatus) {
        guard let id = layout["id"] as? String, !id.isEmpty, id.utf8.count <= 128,
            layout["name"] is String, let root = layout["root"]
        else { throw .invalidArgument }
        var leaves = 0
        try node(root, depth: 1, leaves: &leaves)
    }

    private static func node(_ value: Any, depth: Int, leaves: inout Int) throws(CoreStatus) {
        guard depth <= maxDepth, let n = value as? [String: Any] else { throw .invalidArgument }
        if let split = n["split"] {
            guard let axis = split as? String, axis == "row" || axis == "col",
                let ratio = (n["ratio"] as? NSNumber)?.doubleValue, ratio > 0, ratio < 1,
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
            !device.isEmpty, let channel = camera["channel"] as? NSNumber,
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
