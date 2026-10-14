@testable import NVRViewApp
import CryptoKit
import Foundation
import NVRCore

/// Self-test step `updates-bundled-defaults` (docs/UPDATES.md, "Bundled
/// defaults"): an `update-defaults.json` in a throwaway app folder, with a
/// throwaway key made here and a throwaway folder as the source, is read by
/// the real `Updater`; the key is trusted and the source used, Settings'
/// rows show both as bundled, the user's choices override and are the only
/// thing stored, and with no file nothing is trusted. Nothing is fetched.
@MainActor
enum UpdateDefaultsSelfTest {
    static func run(bridge: CoreBridge) -> [String] {
        var problems: [String] = []
        let fm = FileManager.default
        let root = fm.temporaryDirectory.appendingPathComponent("nvrview-updates-selftest-\(UUID().uuidString)")
        defer { try? fm.removeItem(at: root) }
        let withFile = root.appendingPathComponent("with/NVRView.app")
        let without = root.appendingPathComponent("without/NVRView.app")
        let source = root.appendingPathComponent("source").path
        do {
            for app in [withFile, without] {
                try fm.createDirectory(at: app.appendingPathComponent("Contents/Resources"), withIntermediateDirectories: true)
            }
            try fm.createDirectory(atPath: source, withIntermediateDirectories: true)
        } catch {
            return ["updates: could not make the throwaway folders: \(error)"]
        }
        let key = "nvrview-ed25519-pub " + Curve25519.Signing.PrivateKey().publicKey.rawRepresentation
            .map { String(format: "%02x", $0) }.joined()
        let other = "nvrview-ed25519-pub " + Curve25519.Signing.PrivateKey().publicKey.rawRepresentation
            .map { String(format: "%02x", $0) }.joined()
        do {
            try BundledUpdateDefaults(keys: [key], source: source).encoded()
                .write(to: withFile.appendingPathComponent("Contents/Resources/\(BundledUpdateDefaults.fileName)"))
        } catch {
            return ["updates: could not write the defaults file: \(error)"]
        }

        func make(_ app: URL, _ settingsName: String) -> Updater {
            Updater(
                bridge: bridge, store: JSONFileStore(file: root.appendingPathComponent(settingsName)), automaticAllowed: false,
                bundleURL: app, current: "0.3.0")
        }
        func request(_ u: Updater) -> (source: String, keys: [String]) {
            let o = (try? JSONSerialization.jsonObject(with: Data(UpdateRequest.json(u.settings, current: "0.3.0").utf8))) as? [String: Any]
            return (o?["source"] as? String ?? "", o?["keys"] as? [String] ?? [])
        }

        // 1. A bundled file: the key is trusted and the source used, both listed as bundled.
        let a = make(withFile, "a.json")
        let bundledUsed = request(a) == (source, [key]) && a.settings.sourceIsBundled
        if !a.settings.canCheck { problems.append("updates: a bundled key and source did not allow a check") }
        if request(a) != (source, [key]) { problems.append("updates: the check request does not carry the bundled key and source") }
        if a.settings.keyRows.first?.note != "Bundled with this build" || a.settings.keyRows.first?.isBundled != true {
            problems.append("updates: Settings does not list the key as bundled with this build")
        }
        if a.settings.sourceNote != "Bundled default" { problems.append("updates: Settings does not show the source as the bundled default") }
        if a.keyID(key) == "?" { problems.append("updates: the core could not read the bundled key") }

        // 2. The user's choices win. A bundled key is turned off, never deleted; a user key counts alone.
        a.setBundledKey(key, on: false)
        if a.settings.canCheck || a.settings.keyRows.count != 1 { problems.append("updates: a bundled key turned off still trusts or vanished") }
        if a.addKey(other) != nil { problems.append("updates: a user key was refused") }
        if request(a).keys != [other] { problems.append("updates: a turned-off bundled key was still sent") }
        a.removeKey(key)
        if a.settings.keyRows.first?.isBundled != true { problems.append("updates: Remove deleted a bundled key") }
        let mine = root.appendingPathComponent("mine").path
        if a.setSource(mine) != nil { problems.append("updates: a folder source was refused") }
        if request(a).source != mine || a.settings.sourceIsBundled { problems.append("updates: the user's source did not override the bundled one") }
        // Only the user's choices are stored; the bundled file is read again at each start.
        let again = make(withFile, "a.json")
        if again.settings.source != mine || again.settings.keys != [other] || again.settings.disabledBundledKeys != [key] {
            problems.append("updates: the user's choices were not kept")
        }
        if again.settings.bundled.keys != [key] { problems.append("updates: the bundled key was not read again at the next start") }
        _ = again.setSource("")
        if request(again).source != source || !again.settings.sourceIsBundled {
            problems.append("updates: clearing the source did not return to the bundled default")
        }

        // 3. No file: nothing is trusted and nothing is checked.
        let b = make(without, "b.json")
        if b.settings.canCheck || !b.settings.bundled.isEmpty || !b.settings.keyRows.isEmpty || request(b) != ("", []) {
            problems.append("updates: a build without a defaults file trusts or uses something")
        }
        print("updates: bundled key and source used \(bundledUsed), no file trusts nothing \(!b.settings.canCheck), \(problems.count) problems")
        return problems
    }
}
