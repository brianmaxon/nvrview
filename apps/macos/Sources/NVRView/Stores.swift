import Foundation
import NVRCore

// What the app keeps between launches besides layouts (which the core
// stores): the configured devices and the camera groups, as small JSON files
// in the app's data directory, and the passwords, which only ever go to the
// Keychain (docs/SANITIZATION.md "Credentials at runtime").

/// A configured device. No secret in here: the password lives in the
/// credential store under `key`.
struct DeviceConfig: Codable, Equatable, Identifiable, Sendable {
    /// The stable device key layouts name cameras by (`CameraRef.device`).
    var key: String
    var name: String
    var host: String
    var port: UInt16
    var user: String
    /// The core's stable key for this device (ABI 3 `nv_device_key`), once
    /// known; layouts and groups name its cameras by it.
    var coreKey: String?

    var id: String { key }

    static func newKey() -> String { "dev-" + UUID().uuidString.lowercased().prefix(12) }
}

/// A named set of cameras; showing a group lays them out as an even grid.
struct CameraGroup: Codable, Equatable, Identifiable, Sendable {
    var id: String
    var name: String
    var cameras: [CameraRef]

    static func newID() -> String { "group-" + UUID().uuidString.lowercased().prefix(8) }
}

/// Where passwords are kept.
@MainActor
protocol CredentialStore {
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
final class KeychainCredentials: CredentialStore {
    let service: String
    private var cache: [String: String] = [:]

    init(service: String) { self.service = service }

    func password(for key: String) -> String? {
        if let cached = cache[key] { return cached }
        let read = KeychainPassword(service: service, account: key).read()
        if let read { cache[key] = read }
        return read
    }

    func hasPassword(for key: String) -> Bool {
        cache[key] != nil || KeychainPassword(service: service, account: key).exists()
    }

    func setPassword(_ password: String, for key: String) -> Bool {
        let ok = KeychainPassword(service: service, account: key).save(password) == errSecSuccess
        if ok { cache[key] = password }
        return ok
    }

    func deletePassword(for key: String) {
        cache[key] = nil
        KeychainPassword(service: service, account: key).delete()
    }
}

/// For `--fake`: kept in memory only, so development never writes made-up
/// passwords into the user's Keychain.
@MainActor
final class MemoryCredentials: CredentialStore {
    private var passwords: [String: String] = [:]
    func password(for key: String) -> String? { passwords[key] }
    func hasPassword(for key: String) -> Bool { passwords[key] != nil }
    func setPassword(_ password: String, for key: String) -> Bool {
        passwords[key] = password
        return true
    }
    func deletePassword(for key: String) { passwords[key] = nil }
}

/// A list of values in one JSON file, written atomically; or in memory only
/// when `file` is nil.
struct JSONFileStore<Value: Codable> {
    let file: URL?

    func load() -> [Value] {
        guard let file, let data = try? Data(contentsOf: file) else { return [] }
        return (try? JSONDecoder().decode([Value].self, from: data)) ?? []
    }

    func save(_ values: [Value]) {
        guard let file else { return }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let data = try? encoder.encode(values) else { return }
        try? FileManager.default.createDirectory(
            at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: file, options: .atomic)
    }

    /// `~/Library/Application Support/NVRView/<name>`.
    static func appSupport(_ name: String) -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("NVRView", isDirectory: true)
            .appendingPathComponent(name)
    }
}
