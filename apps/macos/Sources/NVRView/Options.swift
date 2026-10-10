import Foundation
import NVRCore

/// Command-line options. The password is deliberately not one of them: argv
/// is visible to every process on the machine. It comes from the Keychain
/// (or the settings sheet, which stores it there).
///
///     NVRView --host H [--port 9000] --user U [--channel 0] [--tier main|sub|mobile|extern]
///             [--keychain-service nvrview] [--keychain-account ACCOUNT]
///     NVRView --fake [--channel N] [--tier T] file.h264|file.h265 ...
///     either form: [--selftest SECONDS]   (window opens behind all others, no focus)
struct Options {
    var host: String?
    var port: UInt16 = 9000
    var user: String?
    var channel: UInt8 = 0
    var tier = StreamTier.main
    var keychainService = "nvrview"
    var keychainAccount: String?
    var fake = false
    var files: [URL] = []
    var selfTestSeconds: Double?

    static let usage = """
        usage: NVRView --host HOST [--port N] --user USER [--channel N] [--tier main|sub|mobile|extern]
                       [--keychain-service NAME] [--keychain-account NAME] [--selftest SECONDS]
               NVRView --fake [--channel N] [--tier TIER] [--selftest SECONDS] file.h264|file.h265 ...
        The password is read from the macOS Keychain (generic password; service defaults to "nvrview",
        account to "USER@HOST"). If it is missing, a settings sheet asks for it and can store it.

        """

    static func parse(_ args: [String]) -> Options? {
        var o = Options()
        var it = args.dropFirst().makeIterator()
        while let a = it.next() {
            switch a {
            case "--host": guard let v = it.next() else { return nil }; o.host = v
            case "--port": guard let v = it.next().flatMap(UInt16.init), v > 0 else { return nil }; o.port = v
            case "--user": guard let v = it.next() else { return nil }; o.user = v
            case "--channel": guard let v = it.next().flatMap(UInt8.init) else { return nil }; o.channel = v
            case "--tier": guard let v = it.next().flatMap(StreamTier.init(name:)) else { return nil }; o.tier = v
            case "--keychain-service": guard let v = it.next(), !v.isEmpty else { return nil }; o.keychainService = v
            case "--keychain-account": guard let v = it.next(), !v.isEmpty else { return nil }; o.keychainAccount = v
            case "--fake": o.fake = true
            case "--selftest": guard let v = it.next().flatMap(Double.init), v > 0 else { return nil }; o.selfTestSeconds = v
            default:
                if a.hasPrefix("--") { return nil }
                o.files.append(URL(fileURLWithPath: a))
            }
        }
        if o.fake { return o.files.isEmpty ? nil : o }
        return o.files.isEmpty ? o : nil
    }

    /// The Keychain account: explicit, or `user@host`.
    var account: String? {
        keychainAccount ?? ((user != nil && host != nil) ? "\(user!)@\(host!)" : nil)
    }
}
