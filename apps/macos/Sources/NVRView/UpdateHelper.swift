import Darwin
import Foundation
import NVRCore

/// `NVRView --apply-update ...`: the helper the app starts as it quits with
/// an update waiting (docs/UPDATES.md, "Applying", macOS). No window, no
/// core. It waits for the app to exit, installs the staged bundle
/// (`BundleInstaller.install`), and with `--relaunch` opens the new app.
///
///     NVRView --apply-update --pid PID --staged DIR --target APP
///             --version VERSION --update-folder DIR [--relaunch]
///             [--wait SECONDS]
///
/// It writes what it did to `<update folder>/update.log` and the outcome to
/// `<update folder>/result.txt`, which the app reads at its next start.
enum UpdateHelper {
    static func run(_ args: [String]) -> Int32 {
        var values: [String: String] = [:]
        var relaunch = false
        var it = args.makeIterator()
        while let a = it.next() {
            switch a {
            case "--apply-update": break
            case "--relaunch": relaunch = true
            case "--pid", "--staged", "--target", "--version", "--update-folder", "--wait":
                guard let v = it.next() else { return 2 }
                values[a] = v
            default: return 2
            }
        }
        guard let pidText = values["--pid"], let pid = pid_t(pidText), let staged = values["--staged"],
            let target = values["--target"], let version = values["--version"], let folder = values["--update-folder"]
        else { return 2 }
        let wait = values["--wait"].flatMap(Double.init) ?? 120
        let folderURL = URL(fileURLWithPath: folder, isDirectory: true)
        let logURL = folderURL.appendingPathComponent("update.log")
        func log(_ line: String) {
            let stamp = ISO8601DateFormatter().string(from: Date())
            let text = "\(stamp) \(line)\n"
            if let h = try? FileHandle(forWritingTo: logURL) {
                h.seekToEndOfFile()
                h.write(Data(text.utf8))
                try? h.close()
            } else {
                try? text.write(to: logURL, atomically: true, encoding: .utf8)
            }
        }
        func result(_ text: String) {
            try? text.write(to: folderURL.appendingPathComponent("result.txt"), atomically: true, encoding: .utf8)
        }
        log("installing \(version)")
        do {
            let app = try BundleInstaller.install(
                staged: URL(fileURLWithPath: staged, isDirectory: true),
                target: URL(fileURLWithPath: target, isDirectory: true), expectedVersion: version,
                updateFolder: folderURL,
                waitForExit: { waitForExit(pid, seconds: wait) },
                verifySignature: verifySignature, log: log)
            result("installed \(version)")
            if relaunch {
                let open = Process()
                open.executableURL = URL(fileURLWithPath: "/usr/bin/open")
                open.arguments = [app.path]
                try? open.run()
                open.waitUntilExit()
                log("reopened \(app.lastPathComponent)")
            }
            return 0
        } catch {
            result("failed \(error)")
            return 1
        }
    }

    /// Polls until `pid` has exited, for at most `seconds`.
    static func waitForExit(_ pid: pid_t, seconds: Double) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if kill(pid, 0) != 0 && errno == ESRCH { return true }
            Thread.sleep(forTimeInterval: 0.1)
        }
        return false
    }

    /// `codesign --verify --deep --strict`: the bundle is intact and signed
    /// (ad hoc or with an identity).
    static func verifySignature(_ bundle: URL) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
        p.arguments = ["--verify", "--deep", "--strict", bundle.path]
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return false }
        p.waitUntilExit()
        return p.terminationStatus == 0
    }
}
