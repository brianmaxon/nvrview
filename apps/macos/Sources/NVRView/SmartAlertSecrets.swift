import AlertHost
import Foundation
import Security
import Synchronization

/// The credentials smart alerts use (an MQTT password, a Home Assistant
/// access token), kept in the Keychain as generic passwords under the app's
/// Keychain service with the account `alerts.<id>`, where `<id>` is the
/// `id` of the input or output in `alerts.json`. Each is read once per
/// launch, kept in memory, and passed to the core with the settings; they
/// are never written to a settings file or a log.
final class KeychainAlertSecrets: AlertSecrets {
    private let service: String
    private let cache = Mutex<[String: String?]>([:])

    init(service: String) { self.service = service }

    static func account(for id: String) -> String { "alerts.\(id)" }

    func secret(for id: String) -> String? {
        if let known = cache.withLock({ $0[id] }) { return known }
        let value = KeychainPassword(service: service, account: Self.account(for: id)).read()
        cache.withLock { $0[id] = .some(value) }
        return value
    }

    /// Stores or replaces a credential (for the settings screen that will
    /// ask for it).
    @discardableResult
    func save(_ value: String, for id: String) -> Bool {
        let ok = KeychainPassword(service: service, account: Self.account(for: id)).save(value) == errSecSuccess
        if ok { cache.withLock { $0[id] = .some(value) } }
        return ok
    }
}
