@testable import NVRViewApp
import AppKit
import Foundation
import NVRCore
import AppBase

/// Self-test step `updates-whats-new` (docs/UX.md, "Release notes"): a
/// synthetic `release-notes.json` in a throwaway app folder is read by the
/// real `Updater`. A first run shows nothing, an upgrade shows that
/// version's notes once, the same version and a downgrade show nothing, a
/// build without the file shows nothing and says so in Help, and the Help
/// list holds every version newest first in a bounded, scrollable view.
/// Nothing is shown on screen and nothing is fetched.
@MainActor
enum WhatsNewSelfTest {
    static func run(bridge: CoreBridge) -> [String] {
        var problems: [String] = []
        let fm = FileManager.default
        let root = fm.temporaryDirectory.appendingPathComponent("nvrview-whatsnew-selftest-\(UUID().uuidString)")
        defer { try? fm.removeItem(at: root) }
        let withFile = root.appendingPathComponent("with/NVRView.app")
        let without = root.appendingPathComponent("without/NVRView.app")
        do {
            for app in [withFile, without] {
                try fm.createDirectory(at: app.appendingPathComponent("Contents/Resources"), withIntermediateDirectories: true)
            }
            let json = #"{"releases":[{"version":"0.1.0","notes":"First made-up notes."},"#
                + #"{"version":"0.3.0","notes":"Newest made-up notes.\n- a bullet\n- another bullet"},"#
                + #"{"version":"0.2.0","notes":"Middle made-up notes."}]}"#
            try Data(json.utf8).write(to: withFile.appendingPathComponent("Contents/Resources/\(BundledReleaseNotes.fileName)"))
        } catch {
            return ["whats-new: could not make the throwaway folders: \(error)"]
        }
        func make(_ app: URL, _ version: String) -> Updater {
            Updater(
                bridge: bridge, store: JSONFileStore(file: root.appendingPathComponent("settings.json")), automaticAllowed: false,
                bundleURL: app, current: version)
        }

        // 1. A first run, then an upgrade, then the same version, then a downgrade.
        let first = make(withFile, "0.2.0")
        if first.releaseNotes.releases.map(\.version) != ["0.3.0", "0.2.0", "0.1.0"] {
            problems.append("whats-new: the bundled notes were not read newest first")
        }
        if first.takeWhatsNew() != nil { problems.append("whats-new: a first run showed the notes") }
        let upgraded = make(withFile, "0.3.0")
        let shown = upgraded.takeWhatsNew()
        if shown?.version != "0.3.0" || shown?.notes.hasPrefix("Newest made-up notes.") != true {
            problems.append("whats-new: an upgrade did not show the new version's notes")
        }
        if make(withFile, "0.3.0").takeWhatsNew() != nil { problems.append("whats-new: the notes were shown twice") }
        if make(withFile, "0.2.0").takeWhatsNew() != nil { problems.append("whats-new: a downgrade showed notes") }
        if make(withFile, "0.3.0").takeWhatsNew()?.version != "0.3.0" {
            problems.append("whats-new: going up again after a downgrade did not show the notes")
        }

        // 2. The Help list: every version, newest first, bullets as bullets, bounded and scrollable.
        let list = upgraded.releaseNotes.displayText
        let at = { (s: String) in list.range(of: s)?.lowerBound }
        if let a = at("Version 0.3.0"), let b = at("Version 0.2.0"), let c = at("Version 0.1.0") {
            if !(a < b && b < c) { problems.append("whats-new: the Help list is not newest first") }
        } else {
            problems.append("whats-new: the Help list lacks a version")
        }
        if !list.contains("\u{2022} a bullet") || list.contains("- a bullet") {
            problems.append("whats-new: a \"- \" line is not shown as a bullet")
        }
        let view = UpdateNotesView.make(notes: String(repeating: list + "\n", count: 6))
        if !view.hasVerticalScroller { problems.append("whats-new: the Help list is not scrollable") }

        // 3. A build without the file: nothing shown, nothing to list, and a dev version is left alone.
        let bare = make(without, "0.4.0")
        if bare.takeWhatsNew() != nil || !bare.releaseNotes.isEmpty { problems.append("whats-new: a build without the file shows notes") }
        if make(withFile, "0.4.0").takeWhatsNew() != nil { problems.append("whats-new: a version without notes showed a window") }
        print("whats-new: upgrade shown \(shown != nil), \(problems.count) problems")
        return problems
    }
}
