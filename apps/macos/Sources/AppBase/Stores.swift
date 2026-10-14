import Foundation
import NVRCore

// What the app keeps between launches besides layouts (which the core
// stores): the configured devices and the camera groups, as small JSON files
// in the app's data directory, and the passwords, which only ever go to the
// Keychain (docs/SANITIZATION.md "Credentials at runtime").

/// A configured device. No secret in here: the password lives in the
/// credential store under `key`.
package struct DeviceConfig: Codable, Equatable, Identifiable, Sendable {
    /// The stable device key layouts name cameras by (`CameraRef.device`).
    package var key: String
    package var name: String
    package var host: String
    package var port: UInt16
    package var user: String
    /// The core's stable key for this device (ABI 3 `nv_device_key`), once
    /// known; layouts and groups name its cameras by it.
    package var coreKey: String?

    package var id: String { key }

    package static func newKey() -> String { "dev-" + UUID().uuidString.lowercased().prefix(12) }

    package init(key: String, name: String, host: String, port: UInt16, user: String, coreKey: String? = nil) {
        self.key = key
        self.name = name
        self.host = host
        self.port = port
        self.user = user
        self.coreKey = coreKey
    }
}

/// A named set of cameras; showing a group lays them out as an even grid.
package struct CameraGroup: Codable, Equatable, Identifiable, Sendable {
    package var id: String
    package var name: String
    package var cameras: [CameraRef]

    package static func newID() -> String { "group-" + UUID().uuidString.lowercased().prefix(8) }

    package init(id: String, name: String, cameras: [CameraRef]) {
        self.id = id
        self.name = name
        self.cameras = cameras
    }
}

/// Where passwords are kept.
@MainActor
package protocol CredentialStore {
    func password(for key: String) -> String?
    /// Whether a password is saved, without reading it (no Keychain prompt).
    func hasPassword(for key: String) -> Bool
    @discardableResult func setPassword(_ password: String, for key: String) -> Bool
    func deletePassword(for key: String)
}

/// The macOS Keychain: one generic password per device key. Each password is
/// read from the Keychain at most once per launch and then kept in memory
/// (never on disk), so reconnects do not ask the Keychain again: every read
/// can show a permission prompt when the app's signature is not trusted yet.
@MainActor
package final class KeychainCredentials: CredentialStore {
    package let service: String
    private var cache: [String: String] = [:]

    package init(service: String) { self.service = service }

    package func password(for key: String) -> String? {
        if let cached = cache[key] { return cached }
        let item = KeychainPassword(service: service, account: key)
        guard let read = item.read() else { return nil }
        cache[key] = read
        // An item saved by an earlier build may trust only that build, so
        // reading it asks every launch. Re-create it once as this app's own
        // (see KeychainPassword.save); only a flag is remembered, never the
        // password.
        let flag = "keychain-owned.\(service).\(key)"
        if !UserDefaults.standard.bool(forKey: flag), item.save(read) == errSecSuccess {
            UserDefaults.standard.set(true, forKey: flag)
        }
        return read
    }

    package func hasPassword(for key: String) -> Bool {
        cache[key] != nil || KeychainPassword(service: service, account: key).exists()
    }

    package func setPassword(_ password: String, for key: String) -> Bool {
        let ok = KeychainPassword(service: service, account: key).save(password) == errSecSuccess
        if ok {
            cache[key] = password
            UserDefaults.standard.set(true, forKey: "keychain-owned.\(service).\(key)")
        }
        return ok
    }

    package func deletePassword(for key: String) {
        cache[key] = nil
        UserDefaults.standard.removeObject(forKey: "keychain-owned.\(service).\(key)")
        KeychainPassword(service: service, account: key).delete()
    }
}

/// For `--fake`: kept in memory only, so development never writes made-up
/// passwords into the user's Keychain.
@MainActor
package final class MemoryCredentials: CredentialStore {
    private var passwords: [String: String] = [:]
    package init() {}
    package func password(for key: String) -> String? { passwords[key] }
    package func hasPassword(for key: String) -> Bool { passwords[key] != nil }
    package func setPassword(_ password: String, for key: String) -> Bool {
        passwords[key] = password
        return true
    }
    package func deletePassword(for key: String) { passwords[key] = nil }
}

/// A list of values in one JSON file, written atomically; or in memory only
/// when `file` is nil.
package struct JSONFileStore<Value: Codable> {
    package let file: URL?

    package func load() -> [Value] {
        guard let file, let data = try? Data(contentsOf: file) else { return [] }
        return (try? JSONDecoder().decode([Value].self, from: data)) ?? []
    }

    package func save(_ values: [Value]) {
        guard let file else { return }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let data = try? encoder.encode(values) else { return }
        try? FileManager.default.createDirectory(
            at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: file, options: .atomic)
    }

    /// `~/Library/Application Support/NVRView/<name>`.
    package static func appSupport(_ name: String) -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("NVRView", isDirectory: true)
            .appendingPathComponent(name)
    }

    package init(file: URL?) {
        self.file = file
    }
}
