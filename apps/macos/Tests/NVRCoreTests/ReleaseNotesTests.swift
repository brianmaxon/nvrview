import Foundation
import NVRCore
import Testing

/// Bundled release notes and the "What's New" decision (docs/UPDATES.md,
/// "Bundled release notes"; docs/UX.md, "Release notes"). Mirrored by
/// NvrView.Shell.Tests on Windows. All text here is made up.
struct ReleaseNotesTests {
    static let file = Data(
        (#"{"releases":[{"version":"0.2.0","notes":"Two."},{"version":"0.10.0","notes":"Ten.\n- x"},"#
            + #"{"version":"0.9.1","notes":"Nine."},{"version":"bad","notes":"No."},{"version":"0.9.1","notes":"Dup."},"#
            + #"{"version":"0.5.0","notes":"   "},{"version":"0.6.0"}]}"#).utf8)

    @Test func versionOrder() {
        #expect(AppVersion.compare("0.10.0", "0.9.9") == 1)
        #expect(AppVersion.compare("1.0", "1.0.0") == 0)
        #expect(AppVersion.compare("0.3.0", "0.3.1") == -1)
        #expect(AppVersion.compare("1.2.3-beta", "1.2.3") == 0)
        for bad in ["", "x", "1..2", "1.a", "1.2.3.4.5", "-1"] { #expect(AppVersion.parts(bad) == nil, "\(bad)") }
    }

    @Test func parsesLeniently() {
        let n = BundledReleaseNotes.parse(Self.file)
        #expect(n.releases.map(\.version) == ["0.10.0", "0.9.1", "0.2.0"])
        #expect(n.releases[1].notes == "Nine.")
        #expect(n.notes(for: "0.2")?.notes == "Two.")
        for bad in ["", "{not json", "[]", #"{"releases":5}"#, #"{"releases":[1,"x"]}"#] {
            #expect(BundledReleaseNotes.parse(Data(bad.utf8)).isEmpty, "\(bad)")
        }
        let long = String(repeating: "a", count: 2500)
        let cut = BundledReleaseNotes.parse(Data(#"{"releases":[{"version":"1.0.0","notes":"\#(long)"}]}"#.utf8))
        #expect(cut.releases.first?.notes.count == 2000)
    }

    @Test func missingFileIsNothing() {
        let nowhere = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-no-such-\(UUID().uuidString)")
        #expect(BundledReleaseNotes.load(from: nowhere).isEmpty)
        #expect(BundledReleaseNotes.load(from: nil).isEmpty)
    }

    @Test func formatsPlainText() {
        #expect(
            ReleaseNotesText.format("Intro.\r\n\r\n\r\n\r\n- one  \n* two\n  - nested")
                == "Intro.\n\n\u{2022} one\n\u{2022} two\n  \u{2022} nested")
        let text = BundledReleaseNotes.parse(Self.file).displayText
        #expect(text.hasPrefix("Version 0.10.0\n\nTen.\n\u{2022} x"))
        #expect(text.contains("Version 0.2.0\n\nTwo."))
    }

    @Test func decisions() {
        let b = BundledReleaseNotes.parse(Self.file)
        typealias D = WhatsNew.Decision
        // Upgrade: notes, and the new version is remembered.
        #expect(WhatsNew.decide(current: "0.10.0", lastRun: "0.9.1", bundled: b) == D(show: b.releases[0], remember: "0.10.0"))
        // Fresh install: no window, remember.
        #expect(WhatsNew.decide(current: "0.10.0", lastRun: nil, bundled: b) == D(show: nil, remember: "0.10.0"))
        // Same version: nothing at all.
        #expect(WhatsNew.decide(current: "0.10.0", lastRun: "0.10.0", bundled: b) == D(show: nil, remember: nil))
        // Downgrade: no window, remember the older one so going up shows it again.
        #expect(WhatsNew.decide(current: "0.2.0", lastRun: "0.10.0", bundled: b) == D(show: nil, remember: "0.2.0"))
        // An upgrade to a version the file lacks, and a build with no file.
        #expect(WhatsNew.decide(current: "0.11.0", lastRun: "0.10.0", bundled: b) == D(show: nil, remember: "0.11.0"))
        #expect(WhatsNew.decide(current: "0.11.0", lastRun: "0.10.0", bundled: .none) == D(show: nil, remember: "0.11.0"))
        // A stored value that is not a version counts as a first run; a development version does nothing.
        #expect(WhatsNew.decide(current: "0.10.0", lastRun: "junk", bundled: b) == D(show: nil, remember: "0.10.0"))
        #expect(WhatsNew.decide(current: "dev", lastRun: "0.1.0", bundled: b) == D(show: nil, remember: nil))
    }

    @Test func lastRunVersionIsStoredWithTheUpdateSettings() throws {
        var s = UpdateSettings()
        s.lastRunVersion = "0.3.0"
        let back = try JSONDecoder().decode(UpdateSettings.self, from: JSONEncoder().encode(s))
        #expect(back.lastRunVersion == "0.3.0")
        #expect(try JSONDecoder().decode(UpdateSettings.self, from: Data("{}".utf8)).lastRunVersion == nil)
    }

    @Test func lineKindsSeparateHeadingsBulletsAndText() {
        let text = ReleaseNotesText.format("Made-up summary sentence.\n\nFixes\n- one\n  - nested\n\nA long line that ends like a sentence, so it is text.\n\nTrailing heading")
        let kinds = ReleaseNotesText.lineKinds(text)
        #expect(kinds == [.text, .blank, .heading, .bullet(level: 0), .bullet(level: 1), .blank, .text, .blank, .heading])
        #expect(ReleaseNotesText.lineKinds(BundledReleaseNotes(releases: [ReleaseNote(version: "1.0.0", notes: "- a")]).displayText).first == .heading)
        let tooLong = String(repeating: "x", count: ReleaseNotesText.maxHeadingLength + 1)
        #expect(ReleaseNotesText.lineKinds(tooLong) == [.text])
    }
}
