import Foundation
import Security

/// A generic-password Keychain item. Service and account are chosen by the
/// caller (command-line options), never the password.
struct KeychainPassword {
    var service: String
    var account: String

    private var query: [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: account]
    }

    /// Whether the item exists, from its attributes only: the secret is not
    /// read, so this never shows a Keychain permission prompt.
    func exists() -> Bool {
        var q = query
        q[kSecReturnAttributes] = true
        q[kSecMatchLimit] = kSecMatchLimitOne
        var out: CFTypeRef?
        return SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess
    }

    /// The stored password, or nil when there is none (or it cannot be read).
    func read() -> String? {
        var q = query
        q[kSecReturnData] = true
        q[kSecMatchLimit] = kSecMatchLimitOne
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let data = out as? Data else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    /// Stores or replaces the password by deleting any existing item and
    /// adding a new one. Updating in place would keep the old item's access
    /// list, which trusts only the build that created it; a new item is
    /// created by this app, and the Keychain trusts its creator (and later
    /// builds with the same signing identity) without asking.
    @discardableResult
    func save(_ password: String) -> OSStatus {
        let removed = SecItemDelete(query as CFDictionary)
        if removed != errSecSuccess && removed != errSecItemNotFound { return removed }
        var add = query
        add[kSecValueData] = Data(password.utf8)
        return SecItemAdd(add as CFDictionary, nil)
    }

    @discardableResult
    func delete() -> OSStatus { SecItemDelete(query as CFDictionary) }
}
