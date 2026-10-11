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
    /// was never applied); keeps `previous` and the log.
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
        case swapFailed(Int32)
    }

    /// Installs `staged/NVRView.app` (`staged` is the core's `ready` path)
    /// in place of `target`:
    /// 1. waits for the app to exit (`waitForExit`, false: give up);
    /// 2. checks the staged bundle: same bundle identifier as the installed
    ///    one, the expected version, and `verifySignature`;
    /// 3. swaps the two in one atomic rename (`renamex_np`, `RENAME_SWAP`);
    /// 4. moves the old bundle to `<update folder>/previous/<name>.app`,
    ///    replacing an older one there.
    /// Nothing is changed unless steps 1 and 2 pass. Returns the installed
    /// bundle's location (`target`).
    @discardableResult
    public static func install(
        staged: URL, target: URL, expectedVersion: String, updateFolder: URL,
        waitForExit: () -> Bool, verifySignature: (URL) -> Bool,
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
        guard verifySignature(new) else {
            log("the staged bundle's code signature did not verify; nothing changed")
            throw .signatureInvalid
        }
        let swapped = new.withUnsafeFileSystemRepresentation { a in
            target.withUnsafeFileSystemRepresentation { b in
                renamex_np(a!, b!, UInt32(RENAME_SWAP))
            }
        }
        guard swapped == 0 else {
            let e = errno
            log("swapping the bundles failed (errno \(e)); nothing changed")
            throw .swapFailed(e)
        }
        // `new` now holds the old bundle.
        let previousFolder = updateFolder.appendingPathComponent("previous", isDirectory: true)
        let previous = previousFolder.appendingPathComponent(target.lastPathComponent)
        try? fileManager.createDirectory(at: previousFolder, withIntermediateDirectories: true)
        try? fileManager.removeItem(at: previous)
        if (try? fileManager.moveItem(at: new, to: previous)) == nil {
            log("installed; the previous version stays at \(new.path)")
        } else {
            log("installed \(expectedVersion); the previous version is in \(previousFolder.path)")
            try? fileManager.removeItem(at: staged.deletingLastPathComponent())
        }
        return target
    }
}
