import Foundation
import NVRCore
import Testing

/// Bundled update keys and source (docs/UPDATES.md, "Bundled defaults"):
/// reading `update-defaults.json`, merging it with the user's settings, and
/// validation. Mirrored by NvrView.Shell.Tests on Windows.
struct BundledUpdateDefaultsTests {
    static let keyA = "nvrview-ed25519-pub " + String(repeating: "ab", count: 32)
    static let keyB = "nvrview-ed25519-pub " + String(repeating: "cd", count: 32)

    @Test func keyAndSourceValidation() {
        #expect(BundledUpdateDefaults.canonicalKey("  nvrview-ed25519-pub  " + String(repeating: "AB", count: 32)) == Self.keyA)
        for bad in [
            "", "nvrview-ed25519-pub", "nvrview-ed25519-pub abcd", "nvrview-ed25519-sec " + String(repeating: "ab", count: 32),
            "nvrview-ed25519-pub " + String(repeating: "zz", count: 32), String(repeating: "ab", count: 32),
        ] {
            #expect(BundledUpdateDefaults.canonicalKey(bad) == nil, "\(bad)")
        }
        for good in [
            "/Volumes/share/nvrview", "https://updates.example.com/nvrview/", "http://192.0.2.10/u", #"C:\updates"#,
            #"\\server\share\nvrview"#,
        ] {
            #expect(BundledUpdateDefaults.isValidSource(good), "\(good)")
        }
        for bad in ["", "relative/path", "ftp://example.com/u", "https://user:pw@example.com/u", "https://", " /x", "file:///x"] {
            #expect(!BundledUpdateDefaults.isValidSource(bad), "\(bad)")
        }
    }

    @Test func parsesTheFileLeniently() {
        let json = #"{"keys":["\#(Self.keyA)","junk","\#(Self.keyA)","\#(Self.keyB)"],"source":"/srv/updates"}"#
        let d = BundledUpdateDefaults.parse(Data(json.utf8))
        #expect(d.keys == [Self.keyA, Self.keyB] && d.source == "/srv/updates")
        #expect(BundledUpdateDefaults.parse(Data("{not json".utf8)).isEmpty)
        #expect(BundledUpdateDefaults.parse(Data(#"{"keys":[],"source":"relative"}"#.utf8)).isEmpty)
        // Written and read back.
        #expect(BundledUpdateDefaults.parse(d.encoded()) == d)
    }

    @Test func loadsFromAFolder() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-defaults-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(BundledUpdateDefaults.load(from: dir).isEmpty)
        #expect(BundledUpdateDefaults.load(from: nil).isEmpty)
        try BundledUpdateDefaults(keys: [Self.keyA], source: "/srv/u").encoded()
            .write(to: dir.appendingPathComponent(BundledUpdateDefaults.fileName))
        #expect(BundledUpdateDefaults.load(from: dir) == BundledUpdateDefaults(keys: [Self.keyA], source: "/srv/u"))
    }

    @Test func noBundledDefaultsTrustsNothing() {
        var s = UpdateSettings()
        #expect(!s.canCheck && s.trustedKeys.isEmpty && s.effectiveSource.isEmpty && !s.sourceIsBundled)
        s.source = "/srv/mine"
        s.keys = [Self.keyB]
        #expect(s.canCheck && s.trustedKeys == [Self.keyB] && s.effectiveSource == "/srv/mine" && s.sourceNote.isEmpty)
    }

    @Test func bundledKeyAndSourceAreTheDefaults() throws {
        var s = UpdateSettings()
        s.bundled = BundledUpdateDefaults(keys: [Self.keyA], source: "/srv/bundled")
        #expect(s.canCheck && s.trustedKeys == [Self.keyA] && s.effectiveSource == "/srv/bundled")
        #expect(s.sourceIsBundled && s.sourceNote == "Bundled default")
        #expect(s.keyRows == [.init(key: Self.keyA, isBundled: true, isOn: true)] && s.keyRows[0].note == "Bundled with this build")
        let json = try #require(
            try JSONSerialization.jsonObject(with: Data(UpdateRequest.json(s, current: "0.3.0").utf8)) as? [String: Any])
        #expect(json["source"] as? String == "/srv/bundled" && json["keys"] as? [String] == [Self.keyA])
        // The user's source wins; a cleared field returns to the default.
        s.source = " /srv/mine "
        #expect(s.effectiveSource == "/srv/mine" && !s.sourceIsBundled && s.sourceNote.isEmpty)
        s.source = ""
        #expect(s.effectiveSource == "/srv/bundled")
        // A bundled key can be turned off, not deleted; the user's own keys still count.
        s.setBundledKey(Self.keyA, on: false)
        #expect(!s.canCheck && s.trustedKeys.isEmpty && s.keyRows.count == 1 && !s.keyRows[0].isOn)
        s.keys = [Self.keyB, Self.keyA]
        #expect(s.trustedKeys == [Self.keyB] && s.keyRows.map(\.key) == [Self.keyA, Self.keyB])
        s.setBundledKey(Self.keyA, on: true)
        #expect(s.trustedKeys == [Self.keyA, Self.keyB])
        s.setBundledKey(Self.keyB, on: false)
        #expect(s.disabledBundledKeys.isEmpty, "a user key is not a bundled key")
    }

    @Test func onlyTheUsersChoicesAreStored() throws {
        var s = UpdateSettings()
        s.bundled = BundledUpdateDefaults(keys: [Self.keyA], source: "/srv/bundled")
        s.setBundledKey(Self.keyA, on: false)
        s.automatic = true
        let text = String(decoding: try JSONEncoder().encode(s), as: UTF8.self)
        #expect(!text.contains("/srv/bundled") && text.contains("disabledBundledKeys"))
        let back = try JSONDecoder().decode(UpdateSettings.self, from: Data(text.utf8))
        #expect(back.bundled.isEmpty && back.disabledBundledKeys == [Self.keyA] && back.automatic)
        // A file from before bundled defaults existed still loads.
        let old = try JSONDecoder().decode(UpdateSettings.self, from: Data(#"{"source":"/s","keys":["k"],"automatic":true}"#.utf8))
        #expect(old.source == "/s" && old.keys == ["k"] && old.disabledBundledKeys.isEmpty)
    }
}
