import Darwin
import Foundation

// Self-update (docs/UPDATES.md) as the macOS app does it, the parts that
// need no window: the core's `update` events, the settings and their
// schedule, the request JSON, and installing a staged bundle in place of the
// running one (`BundleInstaller`, run by the `--apply-update` helper after
// the app has quit).

/// One `update` event (docs/FFI.md "Updates").
public struct UpdateEvent: Sendable, Equatable, Decodable {
    public var op: UInt64
    public var action: String
    public var state: String
    public var current: String?
    public var version: String?
    public var latest: String?
    public var notes: String?
    public var size: UInt64?
    public var received: UInt64?
    public var total: UInt64?
    public var path: String?
    public var reason: String?
    public var message: String?
    public var no_package: Bool?
    public var key_id: String?
    /// The manifest's signed publish time (`YYYY-MM-DDTHH:MM:SSZ`).
    public var published: String?
    /// `unpublished` for a manifest without a publish time.
    public var warning: String?

    public init(op: UInt64, action: String, state: String) {
        self.op = op
        self.action = action
        self.state = state
    }
}

/// What the user set in Settings, Updates. Kept with the app's settings in
/// the data directory; the updater itself never writes there.
public struct UpdateSettings: Codable, Equatable, Sendable {
    /// A folder or URL; empty: updates are off.
    public var source = ""
    /// Canonical public key lines (`nvrview-ed25519-pub <hex>`).
    public var keys: [String] = []
    /// Check at startup and about every 24 hours.
    public var automatic = false
    /// When the last check finished (any outcome), wall-clock time.
    public var lastCheck: Date?

    public init() {}

    /// A source and at least one key: a check can run.
    public var canCheck: Bool { !source.trimmingCharacters(in: .whitespaces).isEmpty && !keys.isEmpty }
}

/// When automatic checks run: once shortly after launch, then about every
/// 24 hours, counted from the last check (kept across launches). Pure, so
/// tests drive it with made-up dates.
public struct UpdateSchedule: Sendable {
    public var interval: TimeInterval = 24 * 3600
    /// The first check waits this long after launch, so it never competes
    /// with devices connecting.
    public var startupDelay: TimeInterval = 60

    public init() {}

    public func isDue(_ settings: UpdateSettings, now: Date, launchedAt: Date) -> Bool {
        guard settings.automatic, settings.canCheck else { return false }
        guard now.timeIntervalSince(launchedAt) >= startupDelay else { return false }
        guard let last = settings.lastCheck else { return true }
        // A clock set back makes the last check look in the future: due.
        let since = now.timeIntervalSince(last)
        return since >= interval || since < 0
    }
}

/// The updater's state, for the status line and the menu.
public enum UpdateStatus: Equatable, Sendable {
    case idle
    case checking
    case upToDate(latest: String, at: Date)
    case available(version: String, notes: String)
    case downloading(version: String, fraction: Double)
    case ready(version: String, path: String)
    case failed(String)

    /// One understated line for Settings.
    public func line(current: String, formatter: (Date) -> String) -> String {
        switch self {
        case .idle: return "Version \(current)."
        case .checking: return "Checking for updates…"
        case .upToDate(_, let at): return "Up to date (\(current)), checked \(formatter(at))."
        case .available(let v, _): return "Version \(v) is available."
        case .downloading(let v, let f): return "Downloading \(v)… \(Int((f * 100).rounded()))%"
        case .ready(let v, _): return "Version \(v) is ready to install."
        case .failed(let m): return m
        }
    }

    /// Reduces an event of the current operation. Nil: no change.
    public static func after(_ e: UpdateEvent, now: Date, pendingVersion: String?) -> UpdateStatus? {
        switch e.state {
        case "started": return e.action == "check" ? .checking : .downloading(version: pendingVersion ?? "", fraction: 0)
        case "up_to_date": return .upToDate(latest: e.latest ?? "", at: now)
        case "available": return .available(version: e.version ?? "", notes: e.notes ?? "")
        case "downloading":
            let f = (e.total ?? 0) > 0 ? Double(e.received ?? 0) / Double(e.total ?? 1) : 0
            return .downloading(version: pendingVersion ?? "", fraction: min(max(f, 0), 1))
        case "ready":
            guard let v = e.version, let p = e.path else { return .failed("The update could not be prepared.") }
            return .ready(version: v, path: p)
        case "failed": return .failed(UpdateStatus.message(reason: e.reason, detail: e.message))
        case "canceled": return .idle
        default: return nil
        }
    }

    /// A sentence for a failure reason (the core's message as the detail).
    public static func message(reason: String?, detail: String?) -> String {
        let lead: String
        switch reason {
        case "no_trusted_keys": lead = "Add the update publisher's public key first."
        case "bad_signature": lead = "The update's signature did not verify with any trusted key."
        case "insecure_source": lead = "Plain http:// is allowed only on the local network."
        case "source_invalid": lead = "The update source is not a folder or URL NVRView can use."
        case "unreachable": lead = "The update source could not be reached."
        case "not_found": lead = "No update was found at the source."
        case "bad_package", "unsafe_archive": lead = "The update package was refused."
        case "staging": lead = "The update could not be prepared next to the app."
        case "unsupported_cpu": lead = "This computer cannot use HTTPS update sources; use a folder."
        case "expired": lead = "The update source's signed list has expired; its publisher needs to sign it again."
        case "stale_manifest":
            lead = "The update source offers an older signed list than one already seen, so it was refused (a replay or a stale copy)."
        case "not_yet_valid": lead = "The update's publish date is ahead of this Mac's clock; check the date and time."
        case "unpublished": lead = "The update source's signed list has no publish date."
        default: lead = "The update check failed."
        }
        guard let detail, !detail.isEmpty else { return lead }
        return "\(lead) (\(detail))"
    }
}

/// The JSON for `nv_update_check` and `nv_update_download`.
public enum UpdateRequest {
    public static func json(
        _ settings: UpdateSettings, current: String, download: (version: String, staging: String)? = nil
    ) -> String {
        var o: [String: Any] = [
            "source": settings.source.trimmingCharacters(in: .whitespacesAndNewlines),
            "keys": settings.keys,
            "current": current,
        ]
        if let d = download {
            o["version"] = d.version
            o["staging_dir"] = d.staging
        }
        let data = (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys, .withoutEscapingSlashes])) ?? Data()
        return String(decoding: data, as: UTF8.self)
    }
}

/// Why this copy of the app cannot update itself in place.
public enum UpdateInstallBlocker: Error, Equatable, Sendable {
    case notABundle
    case translocated
    case readOnlyVolume
    case notWritable(String)

    public var message: String {
        switch self {
        case .notABundle: return "This copy of NVRView is not an app bundle, so it cannot update itself."
        case .translocated:
            return "macOS is running NVRView from a temporary location. Move NVRView to Applications, open it from there, and update again."
        case .readOnlyVolume: return "NVRView is on a read-only volume (such as a disk image). Copy it to Applications first."
        case .notWritable(let folder): return "NVRView cannot write to \(folder). Move NVRView to a folder you own and update again."
        }
    }
}

/// What the installed bundle's code signature says about who may sign its
/// updates (docs/UPDATES.md, "Applying", macOS), read from `codesign -d
/// -r- --verbose=2`.
public struct CodeSigningInfo: Equatable, Sendable {
    /// Signed ad hoc (no identity): the requirement is the build's own hash.
    public var adHoc: Bool
    /// The signing team, when signed with a developer identity.
    public var teamID: String?
    /// The explicit designated requirement, when it has one.
    public var designated: String?

    public init(adHoc: Bool, teamID: String?, designated: String?) {
        self.adHoc = adHoc
        self.teamID = teamID
        self.designated = designated
    }

    /// Parses codesign's display output (standard output and error
    /// together). Nil when it shows no signature at all.
    public static func parse(_ text: String) -> CodeSigningInfo? {
        var info = CodeSigningInfo(adHoc: false, teamID: nil, designated: nil)
        var signed = false
        for raw in text.split(whereSeparator: \.isNewline) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line == "Signature=adhoc" {
                info.adHoc = true
                signed = true
            } else if line.hasPrefix("TeamIdentifier=") {
                let team = String(line.dropFirst("TeamIdentifier=".count))
                if team != "not set" && !team.isEmpty { info.teamID = team }
                signed = true
            } else if line.hasPrefix("designated => ") {
                info.designated = String(line.dropFirst("designated => ".count))
                signed = true
            } else if line.hasPrefix("Signature size=") || line.hasPrefix("CodeDirectory ") {
                signed = true
            }
        }
        return signed ? info : nil
    }

    /// The rule a staged update must meet.
    public var rule: SignerRule {
        if adHoc { return .anyValidSignature }
        if let designated, !designated.hasPrefix("cdhash ") { return .requirement(designated) }
        if let teamID, teamID.allSatisfy({ $0.isLetter || $0.isNumber }) {
            return .requirement("anchor apple generic and certificate leaf[subject.OU] = \"\(teamID)\"")
        }
        return .unknown
    }
}

/// Who may sign an update of the installed bundle.
public enum SignerRule: Equatable, Sendable {
    /// The installed app is ad hoc signed (or not signed): any bundle whose
    /// signature is intact. The update keys are what protects it.
    case anyValidSignature
    /// The installed app has a developer identity: the update must satisfy
    /// this code requirement (the installed bundle's designated
    /// requirement, or its Team ID).
    case requirement(String)
    /// Signed with an identity whose requirement could not be read:
    /// updates are refused rather than guessed at.
    case unknown

    /// One sentence for Settings.
    public var summary: String {
        switch self {
        case .anyValidSignature:
            return "This copy is signed ad hoc, so an update needs only an intact code signature; your trusted keys are what protect it."
        case .requirement:
            return "Updates must be code signed by the same developer as this copy."
        case .unknown:
            return "This copy's code signature could not be read, so updates will be refused."
        }
    }
}

/// `codesign`, run as a separate process.
public enum CodeSign {
    /// The installed bundle's rule: from its signature, or
    /// `.anyValidSignature` when it is not signed (a development build).
    public static func rule(forInstalled bundle: URL) -> SignerRule {
        let (status, text) = run(["-d", "-r-", "--verbose=2", bundle.path])
        guard let info = CodeSigningInfo.parse(text) else {
            return status == 0 ? .unknown : .anyValidSignature
        }
        return info.rule
    }

    /// `codesign --verify --deep --strict`, with `-R` for a requirement.
    public static func verify(_ bundle: URL, rule: SignerRule) -> Bool {
        var args = ["--verify", "--deep", "--strict"]
        switch rule {
        case .anyValidSignature: break
        case .requirement(let r): args += ["-R", "=\(r)"]
        case .unknown: return false
        }
        return run(args + [bundle.path]).status == 0
    }

    private static func run(_ args: [String]) -> (status: Int32, text: String) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
        p.arguments = args
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        guard (try? p.run()) != nil else { return (-1, "") }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return (p.terminationStatus, String(decoding: data, as: UTF8.self))
    }
}

/// Moving a verified, unpacked bundle into place (docs/UPDATES.md,
/// "Applying", macOS). Everything that touches the installed bundle is here
/// and runs only after the app has quit.
public enum BundleInstaller {
    /// The folder next to the app that holds staging and the previous
    /// version: `<folder of NVRView.app>/.NVRView-update`.
    public static let folderName = ".NVRView-update"

    /// Where this bundle's updates are staged, or why it cannot update.
    public static func updateFolder(for bundle: URL, fileManager: FileManager = .default) -> Result<URL, UpdateInstallBlocker> {
        guard bundle.pathExtension == "app" else { return .failure(.notABundle) }
        if bundle.path.contains("/AppTranslocation/") { return .failure(.translocated) }
        if (try? bundle.resourceValues(forKeys: [.volumeIsReadOnlyKey]))?.volumeIsReadOnly == true {
            return .failure(.readOnlyVolume)
        }
        let parent = bundle.deletingLastPathComponent()
        guard fileManager.isWritableFile(atPath: parent.path), fileManager.isWritableFile(atPath: bundle.path) else {
            return .failure(.notWritable(parent.path))
        }
        return .success(parent.appendingPathComponent(folderName, isDirectory: true))
    }

    /// A fresh staging path inside the update folder (the core creates it).
    public static func newStagingPath(in folder: URL, version: String) -> URL {
        folder.appendingPathComponent("staging-\(version)-\(UUID().uuidString.prefix(8))", isDirectory: true)
    }

    /// Removes staging leftovers (an interrupted download, an update that
    /// was never applied): only `staging-*` folders, which never hold a
    /// rollback copy (`install` keeps that in `previous`); `previous`, the
    /// log, and anything else stay.
    public static func removeLeftovers(in folder: URL, fileManager: FileManager = .default) {
        let items = (try? fileManager.contentsOfDirectory(atPath: folder.path)) ?? []
        for name in items where name.hasPrefix("staging-") {
            try? fileManager.removeItem(at: folder.appendingPathComponent(name))
        }
    }

    /// A bundle's Info.plist, read now.
    static func info(_ bundle: URL) -> [String: Any] {
        guard let data = try? Data(contentsOf: bundle.appendingPathComponent("Contents/Info.plist")) else { return [:] }
        return (try? PropertyListSerialization.propertyList(from: data, format: nil)) as? [String: Any] ?? [:]
    }

    public enum Failure: Error, Equatable {
        case appStillRunning
        case missingBundle
        case wrongBundle(String)
        case signatureInvalid
        /// The update is not signed by the installed app's developer.
        case signerMismatch
        /// The installed app's signing requirement could not be read.
        case signerUnknown
        /// No rollback copy could be kept, so nothing was replaced.
        case rollbackUnavailable
        case swapFailed(Int32)

        /// One sentence for the next start's status line.
        public var message: String {
            switch self {
            case .appStillRunning: return "NVRView did not quit in time."
            case .missingBundle: return "the downloaded or the installed app was missing."
            case .wrongBundle(let what): return "the downloaded app has the wrong \(what)."
            case .signatureInvalid: return "the downloaded app's code signature is not intact."
            case .signerMismatch: return "the downloaded app is not code signed by the developer of this copy."
            case .signerUnknown: return "this copy's code signing requirement could not be read."
            case .rollbackUnavailable: return "the previous version could not be kept for going back, so nothing was replaced."
            case .swapFailed(let e): return "the apps could not be swapped (error \(e))."
            }
        }
    }

    /// Installs `staged/NVRView.app` (`staged` is the core's `ready` path)
    /// in place of `target`:
    /// 1. waits for the app to exit (`waitForExit`, false: give up);
    /// 2. checks the staged bundle: same bundle identifier as the installed
    ///    one, the expected version, an intact signature
    ///    (`verifySignature` with `.anyValidSignature`), and the installed
    ///    bundle's `signerRule` (its designated requirement when it has a
    ///    developer identity; nothing more when it is ad hoc);
    /// 3. swaps the two in one atomic rename (`renamex_np`, `RENAME_SWAP`);
    /// 4. keeps the old bundle as `<update folder>/previous/<name>.app`,
    ///    replacing an older one there: the staged bundle is moved into that
    ///    slot and swapped from there, so the old one lands in it directly;
    ///    when the slot cannot be prepared nothing is replaced
    ///    (`rollbackUnavailable`).
    /// Nothing is changed unless steps 1 and 2 pass. Returns the installed
    /// bundle's location (`target`).
    @discardableResult
    public static func install(
        staged: URL, target: URL, expectedVersion: String, updateFolder: URL,
        waitForExit: () -> Bool, verifySignature: (URL, SignerRule) -> Bool = CodeSign.verify,
        signerRule: (URL) -> SignerRule = CodeSign.rule(forInstalled:),
        fileManager: FileManager = .default, log: (String) -> Void = { _ in }
    ) throws(Failure) -> URL {
        guard waitForExit() else {
            log("the app did not quit in time; nothing changed")
            throw .appStillRunning
        }
        let new = staged.appendingPathComponent(target.lastPathComponent)
        guard fileManager.fileExists(atPath: new.path), fileManager.fileExists(atPath: target.path) else {
            log("staged or installed bundle missing; nothing changed")
            throw .missingBundle
        }
        // Read from disk each time: `Bundle(url:)` caches by path.
        let newInfo = info(new)
        let oldInfo = info(target)
        let newID = newInfo["CFBundleIdentifier"] as? String
        guard let newID, newID == oldInfo["CFBundleIdentifier"] as? String else {
            log("the staged bundle has another identifier; nothing changed")
            throw .wrongBundle("bundle identifier")
        }
        guard newInfo["CFBundleShortVersionString"] as? String == expectedVersion else {
            log("the staged bundle is not version \(expectedVersion); nothing changed")
            throw .wrongBundle("version")
        }
        guard verifySignature(new, .anyValidSignature) else {
            log("the staged bundle's code signature did not verify; nothing changed")
            throw .signatureInvalid
        }
        switch signerRule(target) {
        case .anyValidSignature:
            log("the installed app is signed ad hoc: an intact signature is enough")
        case .requirement(let r):
            guard verifySignature(new, .requirement(r)) else {
                log("the staged bundle is not signed by the installed app's developer; nothing changed")
                throw .signerMismatch
            }
        case .unknown:
            log("the installed app's signing requirement could not be read; nothing changed")
            throw .signerUnknown
        }
        // The rollback slot first: the staged bundle moves into
        // `previous/<name>.app` and is swapped from there, so the old bundle
        // lands directly in its rollback place (never inside a `staging-`
        // folder, which the next launch deletes). If the slot cannot be
        // prepared, nothing is replaced.
        let previousFolder = updateFolder.appendingPathComponent("previous", isDirectory: true)
        let previous = previousFolder.appendingPathComponent(target.lastPathComponent)
        let older = previousFolder.appendingPathComponent(target.lastPathComponent + ".older")
        do {
            try fileManager.createDirectory(at: previousFolder, withIntermediateDirectories: true)
        } catch {
            log("the rollback folder \(previousFolder.path) cannot be made; nothing changed")
            throw .rollbackUnavailable
        }
        try? fileManager.removeItem(at: older)
        let hadOlder = fileManager.fileExists(atPath: previous.path)
        if hadOlder, (try? fileManager.moveItem(at: previous, to: older)) == nil {
            log("the earlier rollback copy cannot be moved aside; nothing changed")
            throw .rollbackUnavailable
        }
        func restoreOlder() {
            if hadOlder { try? fileManager.moveItem(at: older, to: previous) }
        }
        do {
            try fileManager.moveItem(at: new, to: previous)
        } catch {
            restoreOlder()
            log("the update cannot be moved next to the rollback copy; nothing changed")
            throw .rollbackUnavailable
        }
        let swapped = previous.withUnsafeFileSystemRepresentation { a in
            target.withUnsafeFileSystemRepresentation { b in
                renamex_np(a!, b!, UInt32(RENAME_SWAP))
            }
        }
        guard swapped == 0 else {
            let e = errno
            // Put the staged bundle back and the earlier rollback copy too.
            try? fileManager.moveItem(at: previous, to: new)
            restoreOlder()
            log("swapping the bundles failed (errno \(e)); nothing changed")
            throw .swapFailed(e)
        }
        // `previous` now holds the old bundle.
        try? fileManager.removeItem(at: older)
        log("installed \(expectedVersion); the previous version is in \(previousFolder.path)")
        try? fileManager.removeItem(at: staged.deletingLastPathComponent())
        return target
    }
}
