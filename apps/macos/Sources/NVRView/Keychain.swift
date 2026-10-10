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

    /// Stores or replaces the password.
    @discardableResult
    func save(_ password: String) -> OSStatus {
        let data = Data(password.utf8)
        let update = SecItemUpdate(query as CFDictionary, [kSecValueData: data] as CFDictionary)
        if update != errSecItemNotFound { return update }
        var add = query
        add[kSecValueData] = data
        return SecItemAdd(add as CFDictionary, nil)
    }

    @discardableResult
    func delete() -> OSStatus { SecItemDelete(query as CFDictionary) }
}
