import FakeCore
import Foundation
import NVRCore
import Testing

/// Self-update on macOS (docs/UPDATES.md): events, the schedule on made-up
/// dates (never real time), status, requests, and installing a staged
/// bundle over a throwaway one in a temporary folder.
struct UpdateTests {
    @Test func updateEventsParse() throws {
        let json = #"{"type":"update","op":3,"action":"check","state":"available","current":"0.3.0","version":"0.4.0","notes":"Fixes.","size":10,"key_id":"0011223344556677"}"#
        guard case .update(let e) = try #require(CoreEvent.parse(Data(json.utf8))) else {
            Issue.record("not an update event")
            return
        }
        #expect(e.op == 3 && e.state == "available" && e.version == "0.4.0" && e.notes == "Fixes.")
        let failed = #"{"type":"update","op":4,"action":"download","state":"failed","reason":"bad_package","message":"x"}"#
        guard case .update(let f) = try #require(CoreEvent.parse(Data(failed.utf8))) else { return }
        #expect(f.reason == "bad_package")
        let dated = #"{"type":"update","op":5,"action":"check","state":"up_to_date","latest":"0.4.0","published":"2026-10-11T12:00:00Z"}"#
        guard case .update(let d) = try #require(CoreEvent.parse(Data(dated.utf8))) else { return }
        #expect(d.published == "2026-10-11T12:00:00Z" && d.warning == nil)
        let undated = #"{"type":"update","op":6,"action":"check","state":"up_to_date","latest":"0.4.0","warning":"unpublished"}"#
        guard case .update(let u) = try #require(CoreEvent.parse(Data(undated.utf8))) else { return }
        #expect(u.warning == "unpublished")
    }

    @Test func scheduleChecksAfterStartupThenDaily() {
        let launch = Date(timeIntervalSince1970: 1_000_000)
        var s = UpdateSettings()
        let schedule = UpdateSchedule()
        // Off until enabled, with a source and a key.
        #expect(!schedule.isDue(s, now: launch.addingTimeInterval(3600), launchedAt: launch))
        s.automatic = true
        #expect(!schedule.isDue(s, now: launch.addingTimeInterval(3600), launchedAt: launch))
        s.source = "/Volumes/share/nvrview"
        s.keys = ["nvrview-ed25519-pub 00"]
        // Not in the first minute; then due (never checked).
        #expect(!schedule.isDue(s, now: launch.addingTimeInterval(30), launchedAt: launch))
        #expect(schedule.isDue(s, now: launch.addingTimeInterval(61), launchedAt: launch))
        // Checked an hour ago: not again until 24 hours have passed.
        s.lastCheck = launch.addingTimeInterval(-3600)
        #expect(!schedule.isDue(s, now: launch.addingTimeInterval(61), launchedAt: launch))
        #expect(!schedule.isDue(s, now: launch.addingTimeInterval(22 * 3600), launchedAt: launch))
        #expect(schedule.isDue(s, now: launch.addingTimeInterval(23 * 3600 + 1), launchedAt: launch))
        // A clock set back: due.
        s.lastCheck = launch.addingTimeInterval(7200)
        #expect(schedule.isDue(s, now: launch.addingTimeInterval(120), launchedAt: launch))
        s.automatic = false
        #expect(!schedule.isDue(s, now: launch.addingTimeInterval(48 * 3600), launchedAt: launch))
    }

    @Test func statusFollowsEvents() {
        let now = Date(timeIntervalSince1970: 0)
        func e(_ action: String, _ state: String, _ edit: (inout UpdateEvent) -> Void = { _ in }) -> UpdateEvent {
            var x = UpdateEvent(op: 1, action: action, state: state)
            edit(&x)
            return x
        }
        #expect(UpdateStatus.after(e("check", "started"), now: now, pendingVersion: nil) == .checking)
        #expect(
            UpdateStatus.after(e("check", "available") { $0.version = "0.4.0"; $0.notes = "n" }, now: now, pendingVersion: nil)
                == .available(version: "0.4.0", notes: "n"))
        #expect(
            UpdateStatus.after(e("download", "downloading") { $0.received = 50; $0.total = 200 }, now: now, pendingVersion: "0.4.0")
                == .downloading(version: "0.4.0", fraction: 0.25))
        #expect(
            UpdateStatus.after(e("download", "ready") { $0.version = "0.4.0"; $0.path = "/x" }, now: now, pendingVersion: "0.4.0")
                == .ready(version: "0.4.0", path: "/x"))
        guard case .failed(let m) = UpdateStatus.after(e("check", "failed") { $0.reason = "bad_signature" }, now: now, pendingVersion: nil)
        else {
            Issue.record("not failed")
            return
        }
        #expect(m.contains("signature"))
        for reason in ["expired", "stale_manifest", "not_yet_valid", "unpublished"] {
            #expect(UpdateStatus.message(reason: reason, detail: nil) != UpdateStatus.message(reason: nil, detail: nil), "\(reason)")
        }
        #expect(UpdateStatus.after(e("check", "canceled"), now: now, pendingVersion: nil) == .idle)
        #expect(UpdateStatus.after(e("check", "later-state"), now: now, pendingVersion: nil) == nil)
    }

    @Test func requestJSON() throws {
        var s = UpdateSettings()
        s.source = " https://updates.example.com/u/ "
        s.keys = ["k1"]
        let check = try #require(
            try JSONSerialization.jsonObject(with: Data(UpdateRequest.json(s, current: "0.3.0").utf8)) as? [String: Any])
        #expect(check["source"] as? String == "https://updates.example.com/u/")
        #expect(check["current"] as? String == "0.3.0" && check["version"] == nil)
        let dl = try #require(
            try JSONSerialization.jsonObject(
                with: Data(UpdateRequest.json(s, current: "0.3.0", download: ("0.4.0", "/a/b")).utf8)) as? [String: Any])
        #expect(dl["version"] as? String == "0.4.0" && dl["staging_dir"] as? String == "/a/b")
    }

    @Test func fakeCoreModelsChecks() throws {
        var options = FakeCore.Options(clips: [])
        options.updateOffer = FakeUpdateOffer(version: "0.4.0", notes: "Fixes.")
        let events = EventLog()
        let core = try FakeCore.factory(options: options)(events)
        #expect(core.updateParseKey("nope") == nil)
        let key = try #require(core.updateParseKey(String(repeating: "ab", count: 32)))
        #expect(key.contains("\"id\":"))
        #expect(core.updateParseSource("http://updates.example.com/").contains("insecure_source"))
        #expect(throws: CoreStatus.invalidArgument) { try core.updateCheck(json: "{") }
        try core.updateCheck(json: #"{"source":"/s","keys":["k"],"current":"0.3.0"}"#)
        #expect(events.updates.map(\.state) == ["started", "available"])
        core.free()
    }

    // MARK: Installing over a throwaway bundle

    /// A minimal bundle: Info.plist and an executable script, ad hoc signed
    /// when `sign` is true.
    private func makeBundle(
        at url: URL, id: String = "io.example.UpdateTest", version: String, sign: Bool = false, identity: String = "-"
    ) throws {
        let fm = FileManager.default
        try fm.createDirectory(at: url.appendingPathComponent("Contents/MacOS"), withIntermediateDirectories: true)
        let plist: [String: Any] = [
            "CFBundleIdentifier": id, "CFBundleShortVersionString": version, "CFBundleExecutable": "UpdateTest",
            "CFBundlePackageType": "APPL", "CFBundleName": "UpdateTest",
        ]
        let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try data.write(to: url.appendingPathComponent("Contents/Info.plist"))
        let exe = url.appendingPathComponent("Contents/MacOS/UpdateTest")
        try "#!/bin/sh\necho \(version)\n".write(to: exe, atomically: true, encoding: .utf8)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: exe.path)
        if sign {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
            p.arguments = ["--force", "--sign", identity, url.path]
            p.standardError = FileHandle.nullDevice
            try p.run()
            p.waitUntilExit()
            #expect(p.terminationStatus == 0)
        }
    }

    private func version(_ bundle: URL) -> String? {
        (NSDictionary(contentsOf: bundle.appendingPathComponent("Contents/Info.plist")) as? [String: Any])?[
            "CFBundleShortVersionString"] as? String
    }

    private func scene() throws -> (root: URL, target: URL, folder: URL, staged: URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-update-\(UUID().uuidString)")
        let target = root.appendingPathComponent("Apps/UpdateTest.app")
        try makeBundle(at: target, version: "0.3.0")
        let folder = try BundleInstaller.updateFolder(for: target).get()
        let staged = folder.appendingPathComponent("staging-0.4.0-test/content")
        return (root, target, folder, staged)
    }

    @Test func installSwapsAndKeepsThePrevious() throws {
        let s = try scene()
        defer { try? FileManager.default.removeItem(at: s.root) }
        #expect(s.folder.lastPathComponent == ".NVRView-update")
        try makeBundle(at: s.staged.appendingPathComponent("UpdateTest.app"), version: "0.4.0")
        var waited = false
        try BundleInstaller.install(
            staged: s.staged, target: s.target, expectedVersion: "0.4.0", updateFolder: s.folder,
            waitForExit: {
                waited = true
                return true
            }, verifySignature: { _, _ in true }, signerRule: { _ in .anyValidSignature })
        #expect(waited)
        #expect(version(s.target) == "0.4.0")
        #expect(version(s.folder.appendingPathComponent("previous/UpdateTest.app")) == "0.3.0")
        #expect(!FileManager.default.fileExists(atPath: s.staged.path))
    }

    @Test func nothingChangesUnlessEveryCheckPasses() throws {
        let s = try scene()
        defer { try? FileManager.default.removeItem(at: s.root) }
        let new = s.staged.appendingPathComponent("UpdateTest.app")
        try makeBundle(at: new, version: "0.4.0")
        func attempt(_ expected: String, wait: Bool = true, verify: Bool = true) -> BundleInstaller.Failure? {
            do {
                try BundleInstaller.install(
                    staged: s.staged, target: s.target, expectedVersion: expected, updateFolder: s.folder,
                    waitForExit: { wait }, verifySignature: { _, _ in verify },
                    signerRule: { _ in .anyValidSignature })
                return nil
            } catch {
                return error
            }
        }
        #expect(attempt("0.4.0", wait: false) == .appStillRunning)
        #expect(attempt("0.5.0") == .wrongBundle("version"))
        #expect(attempt("0.4.0", verify: false) == .signatureInvalid)
        try FileManager.default.removeItem(at: new)
        try makeBundle(at: new, id: "io.example.Other", version: "0.4.0")
        #expect(attempt("0.4.0") == .wrongBundle("bundle identifier"))
        try FileManager.default.removeItem(at: new)
        #expect(attempt("0.4.0") == .missingBundle)
        // The installed bundle was never touched.
        #expect(version(s.target) == "0.3.0")
    }

    @Test func blockedLocations() throws {
        let fm = FileManager.default
        #expect(BundleInstaller.updateFolder(for: URL(fileURLWithPath: "/usr/local/bin/tool")) == .failure(.notABundle))
        #expect(
            BundleInstaller.updateFolder(
                for: URL(fileURLWithPath: "/private/var/folders/xx/T/AppTranslocation/ABC/d/NVRView.app"))
                == .failure(.translocated))
        let root = fm.temporaryDirectory.appendingPathComponent("nvrview-ro-\(UUID().uuidString)")
        let app = root.appendingPathComponent("NVRView.app")
        try makeBundle(at: app, version: "0.3.0")
        try fm.setAttributes([.posixPermissions: 0o555], ofItemAtPath: root.path)
        defer {
            try? fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: root.path)
            try? fm.removeItem(at: root)
        }
        guard case .failure(.notWritable) = BundleInstaller.updateFolder(for: app) else {
            Issue.record("a read-only folder must block the update")
            return
        }
    }

    @Test func leftoversAreRemovedButPreviousIsKept() throws {
        let s = try scene()
        defer { try? FileManager.default.removeItem(at: s.root) }
        try FileManager.default.createDirectory(at: s.staged, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(
            at: s.folder.appendingPathComponent("previous/UpdateTest.app"), withIntermediateDirectories: true)
        BundleInstaller.removeLeftovers(in: s.folder)
        let left = try FileManager.default.contentsOfDirectory(atPath: s.folder.path)
        #expect(left == ["previous"])
    }

    /// With real ad hoc signatures and `codesign --verify`: a bundle
    /// changed after signing is refused.
    @Test func codesignVerificationCatchesTampering() throws {
        let s = try scene()
        defer { try? FileManager.default.removeItem(at: s.root) }
        let new = s.staged.appendingPathComponent("UpdateTest.app")
        try makeBundle(at: new, version: "0.4.0", sign: true)
        func verify(_ url: URL) -> Bool {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/usr/bin/codesign")
            p.arguments = ["--verify", "--deep", "--strict", url.path]
            p.standardError = FileHandle.nullDevice
            guard (try? p.run()) != nil else { return false }
            p.waitUntilExit()
            return p.terminationStatus == 0
        }
        #expect(verify(new))
        try "#!/bin/sh\necho changed\n".write(
            to: new.appendingPathComponent("Contents/MacOS/UpdateTest"), atomically: false, encoding: .utf8)
        #expect(!verify(new))
        #expect(throws: BundleInstaller.Failure.signatureInvalid) {
            try BundleInstaller.install(
                staged: s.staged, target: s.target, expectedVersion: "0.4.0", updateFolder: s.folder,
                waitForExit: { true }, verifySignature: { url, _ in verify(url) },
                signerRule: { _ in .anyValidSignature })
        }
        #expect(version(s.target) == "0.3.0")
    }

    // MARK: Who may sign the update

    @Test func codesignOutputGivesTheRule() {
        // Output shapes as `codesign -d -r- --verbose=2` prints them, with
        // made-up identifiers.
        let adHoc = """
            Executable=/tmp/A.app/Contents/MacOS/A
            Identifier=io.example.A
            CodeDirectory v=20400 size=200 flags=0x2(adhoc) hashes=1+3 location=embedded
            Signature=adhoc
            TeamIdentifier=not set
            # designated => cdhash H"00112233445566778899aabbccddeeff00112233"
            """
        let developer = """
            Identifier=io.example.A
            CodeDirectory v=20500 size=300 flags=0x10000(runtime) hashes=1+7 location=embedded
            Signature size=4789
            Authority=Apple Development: Example Developer (EXAMPLE123)
            TeamIdentifier=EXAMPLE123
            designated => identifier "io.example.A" and anchor apple generic and certificate leaf[subject.CN] = "Apple Development: Example Developer (EXAMPLE123)"
            """
        let teamOnly = "Signature size=4789\nTeamIdentifier=EXAMPLE123\n"
        let unreadable = "Signature size=4789\nTeamIdentifier=not set\n"
        #expect(CodeSigningInfo.parse(adHoc)?.rule == .anyValidSignature)
        #expect(
            CodeSigningInfo.parse(developer)?.rule
                == .requirement(
                    #"identifier "io.example.A" and anchor apple generic and certificate leaf[subject.CN] = "Apple Development: Example Developer (EXAMPLE123)""#
                ))
        #expect(
            CodeSigningInfo.parse(teamOnly)?.rule
                == .requirement(#"anchor apple generic and certificate leaf[subject.OU] = "EXAMPLE123""#))
        #expect(CodeSigningInfo.parse(unreadable)?.rule == .unknown)
        #expect(CodeSigningInfo.parse("/tmp/A.app: code object is not signed at all") == nil)
    }

    @Test func aDeveloperSignedAppNeedsItsDeveloper() throws {
        let s = try scene()
        defer { try? FileManager.default.removeItem(at: s.root) }
        try makeBundle(at: s.staged.appendingPathComponent("UpdateTest.app"), version: "0.4.0")
        func attempt(_ rule: SignerRule, meets: Bool) -> BundleInstaller.Failure? {
            do {
                try BundleInstaller.install(
                    staged: s.staged, target: s.target, expectedVersion: "0.4.0", updateFolder: s.folder,
                    waitForExit: { true },
                    verifySignature: { _, r in r == .anyValidSignature || meets },
                    signerRule: { _ in rule })
                return nil
            } catch {
                return error
            }
        }
        #expect(attempt(.requirement("anchor apple generic"), meets: false) == .signerMismatch)
        #expect(attempt(.unknown, meets: true) == .signerUnknown)
        #expect(version(s.target) == "0.3.0", "nothing changed")
        #expect(attempt(.requirement("anchor apple generic"), meets: true) == nil)
        #expect(version(s.target) == "0.4.0")
    }

    /// Real `codesign`: an ad hoc installed app accepts any intact ad hoc
    /// update.
    @Test func adHocInstalledAcceptsAnIntactUpdate() throws {
        let s = try scene()
        defer { try? FileManager.default.removeItem(at: s.root) }
        try FileManager.default.removeItem(at: s.target)
        try makeBundle(at: s.target, version: "0.3.0", sign: true)
        #expect(CodeSign.rule(forInstalled: s.target) == .anyValidSignature)
        try makeBundle(at: s.staged.appendingPathComponent("UpdateTest.app"), version: "0.4.0", sign: true)
        try BundleInstaller.install(
            staged: s.staged, target: s.target, expectedVersion: "0.4.0", updateFolder: s.folder,
            waitForExit: { true })
        #expect(version(s.target) == "0.4.0")
    }

    /// Real `codesign` with a developer identity from the login keychain,
    /// when there is one (never printed): an app signed with it refuses an
    /// ad hoc update and accepts one signed with the same identity.
    @Test func developerInstalledRefusesAnAdHocUpdate() throws {
        guard let identity = Self.developmentIdentity() else {
            // No Apple Development identity on this Mac: the rule is covered
            // with stubbed codesign output above.
            return
        }
        let s = try scene()
        defer { try? FileManager.default.removeItem(at: s.root) }
        try FileManager.default.removeItem(at: s.target)
        try makeBundle(at: s.target, version: "0.3.0", sign: true, identity: identity)
        guard case .requirement = CodeSign.rule(forInstalled: s.target) else {
            Issue.record("a developer-signed app must have a requirement")
            return
        }
        let new = s.staged.appendingPathComponent("UpdateTest.app")
        try makeBundle(at: new, version: "0.4.0", sign: true)
        #expect(throws: BundleInstaller.Failure.signerMismatch) {
            try BundleInstaller.install(
                staged: s.staged, target: s.target, expectedVersion: "0.4.0", updateFolder: s.folder,
                waitForExit: { true })
        }
        #expect(version(s.target) == "0.3.0")
        try FileManager.default.removeItem(at: new)
        try makeBundle(at: new, version: "0.4.0", sign: true, identity: identity)
        try BundleInstaller.install(
            staged: s.staged, target: s.target, expectedVersion: "0.4.0", updateFolder: s.folder,
            waitForExit: { true })
        #expect(version(s.target) == "0.4.0")
    }

    /// The SHA-1 of the first valid Apple Development identity, if any.
    private static func developmentIdentity() -> String? {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/security")
        p.arguments = ["find-identity", "-v", "-p", "codesigning"]
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        let text = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        p.waitUntilExit()
        for line in text.split(separator: "\n") where line.contains("\"Apple Development:") {
            let parts = line.split(separator: " ")
            if parts.count > 1, parts[1].count == 40 { return String(parts[1]) }
        }
        return nil
    }
}

/// Collects `update` events from a fake core.
private final class EventLog: CoreSink, @unchecked Sendable {
    private let lock = NSLock()
    private var events: [UpdateEvent] = []
    var updates: [UpdateEvent] { lock.withLock { events } }
    func core(didDeliver action: CoreAction, tile: UInt64) {}
    func core(didEmit json: UnsafeRawBufferPointer) {
        if case .update(let u) = CoreEvent.parse(Data(json)) { lock.withLock { events.append(u) } }
    }
}
