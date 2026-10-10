import Foundation
import NVRCore

/// Command-line options. Devices normally come from Settings (passwords in
/// the Keychain); the password is deliberately never an option: argv is
/// visible to every process on the machine.
///
///     NVRView [--keychain-service nvrview]
///     NVRView --host H [--port 9000] --user U        (adds one device for this run)
///     NVRView --fake file.h264|file.h265 ...         (two made-up NVRs playing the files)
///     any form: [--selftest SECONDS]   (window opens behind all others, no focus)
struct Options {
    var host: String?
    var port: UInt16 = 9000
    var user: String?
    var keychainService = "nvrview"
    var fake = false
    var files: [URL] = []
    var selfTestSeconds: Double?
    /// Where devices, groups and the core's store live (default: the app's
    /// Application Support folder); for development and self-tests.
    var dataDirectory: URL?

    static let usage = """
        usage: NVRView [--keychain-service NAME] [--data-dir DIR] [--selftest SECONDS]
               NVRView --host HOST [--port N] --user USER [--data-dir DIR] [--selftest SECONDS]
               NVRView --fake [--selftest SECONDS] file.h264|file.h265 ...
        Devices are configured in Settings; passwords are kept in the macOS Keychain. With --host, the
        password is read from NVRVIEW_PASSWORD (development against mock-nvr) or the Keychain
        (account "USER@HOST").

        """

    static func parse(_ args: [String]) -> Options? {
        var o = Options()
        var it = args.dropFirst().makeIterator()
        while let a = it.next() {
            switch a {
            case "--host": guard let v = it.next(), !v.isEmpty else { return nil }; o.host = v
            case "--port": guard let v = it.next().flatMap(UInt16.init), v > 0 else { return nil }; o.port = v
            case "--user": guard let v = it.next(), !v.isEmpty else { return nil }; o.user = v
            case "--keychain-service": guard let v = it.next(), !v.isEmpty else { return nil }; o.keychainService = v
            case "--data-dir": guard let v = it.next(), !v.isEmpty else { return nil }; o.dataDirectory = URL(fileURLWithPath: v)
            case "--fake": o.fake = true
            case "--selftest": guard let v = it.next().flatMap(Double.init), v > 0 else { return nil }; o.selfTestSeconds = v
            default:
                if a.hasPrefix("--") { return nil }
                o.files.append(URL(fileURLWithPath: a))
            }
        }
        if (o.host == nil) != (o.user == nil) { return nil }
        if o.fake { return o.files.isEmpty ? nil : o }
        return o.files.isEmpty ? o : nil
    }
}
