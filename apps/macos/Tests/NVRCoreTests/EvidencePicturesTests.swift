import CoreGraphics
import Foundation
import NVRCore
import Testing

/// Pictures taken out of the Events window: names, temporary files, the row
/// icon, splits, and the viewer's keys (docs/UX.md "Events").
@Suite struct EvidencePicturesTests {
    @Test func namesFollowSavePicture() {
        let ms: Int64 = 1_700_000_000_000
        let plain = EvidencePictureFiles.fileName(
            camera: "Example Camera", frameMs: ms, shareSafe: false, format: .png, taken: { _ in false })
        #expect(plain?.hasSuffix(" Example Camera.png") == true)
        let safe = EvidencePictureFiles.fileName(
            camera: "Example Camera", frameMs: ms, shareSafe: true, format: .jpeg, taken: { _ in false })
        #expect(safe?.hasSuffix(" Camera A.jpg") == true)
        let again = EvidencePictureFiles.fileName(
            camera: "Example Camera", frameMs: ms, shareSafe: false, format: .png, taken: { $0 == plain })
        #expect(again?.hasSuffix("Example Camera (2).png") == true)
    }

    @Test func oldTemporaryFilesAreRemoved() throws {
        let now = Date(timeIntervalSince1970: 2_000_000_000)
        let files: [(url: URL, modified: Date)] = [
            (URL(fileURLWithPath: "/x/old"), now.addingTimeInterval(-25 * 3600)),
            (URL(fileURLWithPath: "/x/edge"), now.addingTimeInterval(-24 * 3600)),
            (URL(fileURLWithPath: "/x/new"), now.addingTimeInterval(-3600)),
        ]
        #expect(EvidencePictureFiles.expired(files, now: now).map(\.lastPathComponent) == ["old", "edge"])

        // On disk: a clock chosen relative to the files' own dates.
        let manager = FileManager.default
        let folder = manager.temporaryDirectory.appendingPathComponent("evidence-test-\(UUID().uuidString)")
        try manager.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? manager.removeItem(at: folder) }
        let old = folder.appendingPathComponent("old.png")
        let fresh = folder.appendingPathComponent("fresh.png")
        let drag = folder.appendingPathComponent("drag")
        try Data([1]).write(to: old)
        try Data([1]).write(to: fresh)
        try manager.createDirectory(at: drag, withIntermediateDirectories: true)
        let written = Date(timeIntervalSince1970: 1_000_000_000)
        try manager.setAttributes([.modificationDate: written], ofItemAtPath: old.path)
        try manager.setAttributes([.modificationDate: written], ofItemAtPath: drag.path)
        try manager.setAttributes([.modificationDate: written.addingTimeInterval(20 * 3600)], ofItemAtPath: fresh.path)
        let clock = written.addingTimeInterval(24 * 3600)
        #expect(EvidencePictureFiles.removeOld(in: folder, now: clock.addingTimeInterval(-1)) == 0)
        #expect(EvidencePictureFiles.removeOld(in: folder, now: clock) == 2)
        #expect(!manager.fileExists(atPath: old.path))
        #expect(!manager.fileExists(atPath: drag.path))
        #expect(manager.fileExists(atPath: fresh.path))
    }

    private func set(frames: Int, pinned: Bool, expires: Int64?) -> AlertEvidence {
        let f = (0..<frames).map { #"{"index":\#($0),"at_ms":1,"bytes":10}"# }.joined(separator: ",")
        let e = expires.map(String.init) ?? "null"
        return AlertEvidence(
            JSONValue.parse(#"{"trigger_id":5,"pinned":\#(pinned),"expires_ms":\#(e),"frames":[\#(f)]}"#)!)!
    }

    @Test func badgeShowsOnlyWhileKept() {
        let now: Int64 = 1_000_000
        let hour = Int64(3_600_000)
        let live = EvidenceBadge.info(set(frames: 3, pinned: false, expires: now + hour + 20 * 60_000), nowMs: now)
        #expect(live?.tooltip == "3 pictures, deleted in 1 h 20 min")
        #expect(live?.pinned == false)
        let kept = EvidenceBadge.info(set(frames: 1, pinned: true, expires: now + 6 * 24 * hour), nowMs: now)
        #expect(kept?.tooltip == "1 picture, kept (6 days left)")
        #expect(kept?.pinned == true)
        #expect(EvidenceBadge.info(set(frames: 3, pinned: false, expires: now), nowMs: now) == nil)
        #expect(EvidenceBadge.info(set(frames: 0, pinned: false, expires: now + hour), nowMs: now) == nil)
        #expect(EvidenceBadge.remaining(30_000) == "1 min")
        #expect(EvidenceBadge.remaining(2 * hour) == "2 h")
    }

    @Test func splitsKeepBothPanes() {
        #expect(SplitLayout.clamped(fraction: 0.1, total: 1000, minFirst: 300, minSecond: 200) == 0.3)
        #expect(SplitLayout.clamped(fraction: 0.95, total: 1000, minFirst: 300, minSecond: 200) == 0.8)
        #expect(SplitLayout.clamped(fraction: 0.5, total: 1000, minFirst: 300, minSecond: 200) == 0.5)
        #expect(SplitLayout.clamped(fraction: .nan, total: 1000, minFirst: 300, minSecond: 200) == 0.5)
        #expect(SplitLayout.fraction(at: 250, total: 1000) == 0.25)
    }

    @Test func viewerKeysAndFrameSteps() {
        #expect(EvidenceViewerKey(keyCode: 123) == .previous)
        #expect(EvidenceViewerKey(keyCode: 124) == .next)
        #expect(EvidenceViewerKey(keyCode: 53) == .close)
        #expect(EvidenceViewerKey(keyCode: 0) == nil)
        #expect(EvidenceFrameStep.step(from: 0, by: 1, in: [0, 1, 3]) == 1)
        #expect(EvidenceFrameStep.step(from: 1, by: 1, in: [0, 1, 3]) == 3)
        #expect(EvidenceFrameStep.step(from: 3, by: 1, in: [0, 1, 3]) == 3)
        #expect(EvidenceFrameStep.step(from: 0, by: -1, in: [0, 1, 3]) == 0)
        #expect(EvidenceFrameStep.step(from: 9, by: 1, in: [0, 1, 3]) == 0)
    }
}
