import Foundation

/// Where credentials come from (the Keychain in the app, memory in tests).
/// Read at startup and passed to the core in memory; never written anywhere
/// else.
public protocol AlertSecrets: Sendable {
    /// The credential kept for an input's or output's id, if there is one.
    func secret(for id: String) -> String?
}

/// A fixed set of credentials, for tests and the self-test.
public struct MemorySecrets: AlertSecrets {
    public var values: [String: String]
    public init(_ values: [String: String] = [:]) { self.values = values }
    public func secret(for id: String) -> String? { values[id] }
}

public enum AlertSettingsError: Error, Equatable {
    case notAnObject
}

/// The settings document the core is configured with (docs/FFI.md, "Smart
/// alerts"), built from the user's stored `alerts.json`:
///
/// - cameras may be named by `device_key` (the device's stable key) wherever
///   the core wants this run's `device` ID; an entry whose device is not
///   added is left out;
/// - `utc_offset_min` is the computer's current offset;
/// - `secrets` is built from the credential store for every `mqtt` input and
///   every `home_assistant` output (keyed by the entry's `id`, an output's
///   `kind` when it has none). A `secrets` member in the stored file is
///   ignored: credentials are never kept there.
public enum AlertSettingsDocument {
    public struct Built: Sendable, Equatable {
        /// The JSON for `alertsConfigure`.
        public var json: String
        /// The stored document's `enabled` (smart alerts are off unless it
        /// says `true`).
        public var enabled: Bool
        /// Entries left out because their device is not added.
        public var skipped: Int
    }

    public static func build(
        stored: Data, deviceID: (String) -> UInt64?, utcOffsetMinutes: Int, secrets: any AlertSecrets
    ) throws(AlertSettingsError) -> Built {
        guard let root = (try? JSONSerialization.jsonObject(with: stored)) as? [String: Any] else {
            throw .notAnObject
        }
        var skipped = 0
        var doc: [String: Any] = [:]
        for (key, value) in root where key != "secrets" {
            if let resolved = resolve(value, deviceID: deviceID, skipped: &skipped) { doc[key] = resolved }
        }
        let enabled = (root["enabled"] as? Bool) ?? false
        doc["enabled"] = enabled
        doc["utc_offset_min"] = utcOffsetMinutes
        var found: [String: String] = [:]
        for entry in (doc["inputs"] as? [[String: Any]]) ?? [] where (entry["kind"] as? String) == "mqtt" {
            if let id = entry["id"] as? String, let s = secrets.secret(for: id) { found[id] = s }
        }
        for entry in (doc["outputs"] as? [[String: Any]]) ?? [] where (entry["kind"] as? String) == "home_assistant" {
            let id = (entry["id"] as? String) ?? "home_assistant"
            if let s = secrets.secret(for: id) { found[id] = s }
        }
        if !found.isEmpty { doc["secrets"] = found }
        let data = (try? JSONSerialization.data(withJSONObject: doc, options: [.sortedKeys])) ?? Data("{}".utf8)
        return Built(json: String(decoding: data, as: UTF8.self), enabled: enabled, skipped: skipped)
    }

    /// Replaces `device_key` with `device`; nil when an object names a device
    /// that is not added (the nearest array element holding it is dropped).
    private static func resolve(_ value: Any, deviceID: (String) -> UInt64?, skipped: inout Int) -> Any? {
        if let object = value as? [String: Any] {
            var out: [String: Any] = [:]
            for (key, inner) in object {
                if key == "device_key" {
                    guard let k = inner as? String, let id = deviceID(k) else { return nil }
                    out["device"] = id
                } else {
                    guard let r = resolve(inner, deviceID: deviceID, skipped: &skipped) else { return nil }
                    out[key] = r
                }
            }
            return out
        }
        if let array = value as? [Any] {
            var out: [Any] = []
            for element in array {
                if let r = resolve(element, deviceID: deviceID, skipped: &skipped) {
                    out.append(r)
                } else {
                    skipped += 1
                }
            }
            return out
        }
        return value
    }
}
