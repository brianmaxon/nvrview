import AppKit
import Foundation
import NVRCore
import AppBase

/// Self-update from the user's own source (docs/UPDATES.md): the settings,
/// the schedule, the core's check and download, the prompts, and handing the
/// staged bundle to the `--apply-update` helper when the app quits.
///
/// Nothing happens until the user sets a source and a key. Automatic checks
/// never open a window: they change the menu item and the Settings line. A
/// check the user asked for answers with an alert.
@MainActor
@Observable
final class Updater {
    /// A question or answer for the user (shown by the app delegate).
    enum Prompt: Equatable {
        case available(version: String, notes: String)
        case ready(version: String)
        case upToDate(version: String)
        case failed(String)
    }

    @ObservationIgnored private let bridge: CoreBridge
    @ObservationIgnored private let store: JSONFileStore<UpdateSettings>
    @ObservationIgnored private let now: () -> Date
    @ObservationIgnored private let launchedAt: Date
    @ObservationIgnored private let schedule = UpdateSchedule()
    /// Off in self-tests and with `--fake`: no automatic checks.
    @ObservationIgnored let automaticAllowed: Bool
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var userAsked = false
    @ObservationIgnored private var pendingVersion: String?
    @ObservationIgnored private var cachedSignerRule: SignerRule?

    /// Who may code sign an update of this copy, for Settings (read once:
    /// it runs `codesign`).
    var signerSummary: String {
        if cachedSignerRule == nil { cachedSignerRule = CodeSign.rule(forInstalled: bundleURL) }
        return cachedSignerRule?.summary ?? ""
    }
    /// Shows a prompt (set by the app delegate).
    @ObservationIgnored var present: (Prompt) -> Void = { _ in }
    /// Quits the app (Restart Now).
    @ObservationIgnored var quit: () -> Void = { NSApp.terminate(nil) }

    let current: String
    let bundleURL: URL
    private(set) var settings: UpdateSettings
    private(set) var status: UpdateStatus = .idle
    /// Install this staged update when the app quits; relaunch if asked.
    private(set) var installOnQuit: (version: String, path: String, relaunch: Bool)?

    init(
        bridge: CoreBridge, store: JSONFileStore<UpdateSettings>, automaticAllowed: Bool,
        bundleURL: URL = Bundle.main.bundleURL, current: String? = nil, now: @escaping () -> Date = Date.init,
        bundled: BundledUpdateDefaults? = nil, releaseNotes: BundledReleaseNotes? = nil
    ) {
        self.bridge = bridge
        self.store = store
        self.automaticAllowed = automaticAllowed
        self.bundleURL = bundleURL
        self.current = current ?? (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "0.0.0"
        self.now = now
        launchedAt = now()
        self.releaseNotes =
            releaseNotes ?? BundledReleaseNotes.load(from: bundleURL.appendingPathComponent("Contents/Resources", isDirectory: true))
        settings = store.load().first ?? UpdateSettings()
        // What the build bundles (docs/UPDATES.md, "Bundled defaults"): read
        // from the app's Resources on every start, never from the user's file.
        settings.bundled =
            bundled ?? BundledUpdateDefaults.load(from: bundleURL.appendingPathComponent("Contents/Resources", isDirectory: true))
    }

    /// The notes of the versions in this build (docs/UPDATES.md, "Bundled
    /// release notes"), newest first; empty in a development build.
    let releaseNotes: BundledReleaseNotes

    /// The notes to show once on the first launch of a new version, or nil;
    /// records this version as the one that ran.
    func takeWhatsNew() -> ReleaseNote? {
        let decision = WhatsNew.decide(current: current, lastRun: settings.lastRunVersion, bundled: releaseNotes)
        if let remember = decision.remember {
            settings.lastRunVersion = remember
            save()
        }
        return decision.show
    }

    /// Cleans staging leftovers, reports what the last install did, and
    /// starts the schedule (polled every few minutes; the first check comes
    /// a minute after launch).
    func start() {
        if case .success(let folder) = BundleInstaller.updateFolder(for: bundleURL) {
            BundleInstaller.removeLeftovers(in: folder)
            let result = folder.appendingPathComponent("result.txt")
            if let text = try? String(contentsOf: result, encoding: .utf8) {
                try? FileManager.default.removeItem(at: result)
                if text.hasPrefix("failed") { status = .failed("The last update was not installed: \(text.dropFirst(7))") }
            }
        }
        guard automaticAllowed else { return }
        timer = Timer.scheduledTimer(withTimeInterval: 300, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        Timer.scheduledTimer(withTimeInterval: schedule.startupDelay + 1, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    /// Checks if the schedule says so (the timer, and tests with their own
    /// clock).
    func poll() {
        guard schedule.isDue(settings, now: now(), launchedAt: launchedAt) else { return }
        switch status {
        case .checking, .downloading, .ready: return
        default: check(userAsked: false)
        }
    }

    // MARK: Settings

    /// Sets the source; returns a message when the core refuses it.
    @discardableResult
    func setSource(_ text: String) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty, let problem = sourceProblem(trimmed) { return problem }
        settings.source = trimmed
        save()
        return nil
    }

    func sourceProblem(_ text: String) -> String? {
        guard let json = bridge.updateParseSource(text), let o = Self.object(json) else {
            return "This build cannot update itself."
        }
        return (o["ok"] as? Bool) == true ? nil : (o["message"] as? String) ?? "Not a usable source."
    }

    /// Adds a pasted key or a key file's text; returns a message when it is
    /// not a key.
    @discardableResult
    func addKey(_ text: String) -> String? {
        guard let json = bridge.updateParseKey(text), let o = Self.object(json), let key = o["key"] as? String else {
            return "That is not an NVRView update public key."
        }
        if settings.bundled.keys.contains(key) {
            // Adding a bundled key again turns it back on.
            settings.setBundledKey(key, on: true)
            save()
        } else if !settings.keys.contains(key) {
            settings.keys.append(key)
            save()
        }
        return nil
    }

    /// Removes a key the user added. A bundled key cannot be deleted, only
    /// turned off (`setBundledKey`).
    func removeKey(_ key: String) {
        settings.keys.removeAll { $0 == key }
        save()
    }

    func setBundledKey(_ key: String, on: Bool) {
        settings.setBundledKey(key, on: on)
        save()
    }

    /// The short ID shown for a stored key.
    func keyID(_ key: String) -> String {
        bridge.updateParseKey(key).flatMap(Self.object).flatMap { $0["id"] as? String } ?? "?"
    }

    func setAutomatic(_ on: Bool) {
        settings.automatic = on
        save()
    }

    private func save() { store.save([settings]) }

    // MARK: Checking and downloading

    /// The menu item's title.
    var menuTitle: String {
        switch status {
        case .available(let v, _): return "Update to \(v)…"
        case .ready(let v, _): return "Install \(v) and Restart…"
        default: return "Check for Updates…"
        }
    }

    /// The menu item: offer what is known, or check now.
    func menuAction() {
        switch status {
        case .available(let v, let notes): present(.available(version: v, notes: notes))
        case .ready(let v, _): present(.ready(version: v))
        case .checking, .downloading: break
        default: check(userAsked: true)
        }
    }

    func check(userAsked: Bool) {
        guard settings.canCheck else {
            if userAsked { present(.failed("Set an update source and add a trusted key in Settings, Updates.")) }
            return
        }
        self.userAsked = userAsked
        do throws(CoreStatus) {
            try bridge.updateCheck(json: UpdateRequest.json(settings, current: current))
            status = .checking
        } catch .limit {
            // Already checking or downloading.
        } catch {
            status = .failed("This build cannot check for updates.")
            if userAsked { present(.failed(UpdateStatus.message(reason: nil, detail: nil))) }
        }
    }

    /// The user said yes to `version`: download and stage it next to the app.
    func download(_ version: String) {
        let folder: URL
        switch BundleInstaller.updateFolder(for: bundleURL) {
        case .failure(let blocker):
            status = .failed(blocker.message)
            present(.failed(blocker.message))
            return
        case .success(let f): folder = f
        }
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        } catch {
            let blocker = UpdateInstallBlocker.notWritable(folder.deletingLastPathComponent().path)
            status = .failed(blocker.message)
            present(.failed(blocker.message))
            return
        }
        let staging = BundleInstaller.newStagingPath(in: folder, version: version)
        pendingVersion = version
        userAsked = true
        do throws(CoreStatus) {
            try bridge.updateDownload(
                json: UpdateRequest.json(settings, current: current, download: (version, staging.path)))
            status = .downloading(version: version, fraction: 0)
        } catch {
            status = .failed("The download could not start.")
        }
    }

    /// An `update` event from the core.
    func handle(_ e: UpdateEvent) {
        guard let next = UpdateStatus.after(e, now: now(), pendingVersion: pendingVersion) else { return }
        status = next
        let final = !["started", "downloading"].contains(e.state)
        if final, e.action == "check" {
            settings.lastCheck = now()
            save()
        }
        guard final else { return }
        switch next {
        case .available(let v, let notes):
            if userAsked { present(.available(version: v, notes: notes)) }
        case .upToDate(let latest, _):
            if userAsked { present(.upToDate(version: e.current ?? latest)) }
        case .ready(let v, _):
            present(.ready(version: v))
        case .failed(let m):
            if userAsked { present(.failed(m)) }
        default: break
        }
        userAsked = false
    }

    /// Restart Now: install when the app has quit, then reopen it.
    func restartNow() {
        guard case .ready(let v, let path) = status else { return }
        installOnQuit = (v, path, true)
        quit()
    }

    /// Later: install when the user next quits, without reopening.
    func later() {
        guard case .ready(let v, let path) = status else { return }
        installOnQuit = (v, path, false)
    }

    /// The app is quitting: start the helper if an update waits. It waits
    /// for this process to exit before touching anything.
    func appWillTerminate() {
        stop()
        bridge.updateCancel()
        guard let pending = installOnQuit, let exe = Bundle.main.executableURL,
            case .success(let folder) = BundleInstaller.updateFolder(for: bundleURL)
        else { return }
        let p = Process()
        p.executableURL = exe
        p.arguments =
            [
                "--apply-update", "--pid", String(ProcessInfo.processInfo.processIdentifier),
                "--staged", pending.path, "--target", bundleURL.path, "--version", pending.version,
                "--update-folder", folder.path,
            ] + (pending.relaunch ? ["--relaunch"] : [])
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
    }

    private static func object(_ json: String) -> [String: Any]? {
        (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any]
    }
}
