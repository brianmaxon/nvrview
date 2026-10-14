import Foundation
import NVRCore
import Testing

/// The Export tab's decisions (docs/UX.md "Recordings", "Export tab"). The
/// vectors are `fixtures/synthetic/export-panel.json`, which
/// NvrView.Shell.Tests (ExportPanelTests.cs) reads too.
@Suite struct ExportPanelTests {
    private static func vectors() throws -> [String: Any] {
        let data = try Data(contentsOf: Fixtures.directory.appendingPathComponent("export-panel.json"))
        return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private static func list(_ key: String) throws -> [[String: Any]] {
        try #require(try vectors()[key] as? [[String: Any]])
    }

    private static func kind(_ id: Any?) throws -> ExportKind {
        let name = try #require(id as? String)
        return try #require(ExportKind(rawValue: name), "unknown kind \(name)")
    }

    private static func int(_ v: Any?) -> Int64? { (v as? NSNumber)?.int64Value }

    @Test func kindsFollowWhatIsOpen() throws {
        for v in try Self.list("kinds") {
            let c = try #require(v["context"] as? [String: Any])
            let context = ExportContext(
                hasRange: c["hasRange"] as? Bool ?? false, hasPictures: c["hasPictures"] as? Bool ?? false,
                editable: c["editable"] as? Bool ?? false, canWriteEdit: c["canWriteEdit"] as? Bool ?? true,
                sourceCameras: Int(Self.int(c["sourceCameras"]) ?? 0), checkedCameras: Int(Self.int(c["checkedCameras"]) ?? 0))
            let want = try (v["kinds"] as? [String] ?? []).map { try Self.kind($0) }
            #expect(ExportPanelRules.kinds(context) == want, "\(v["note"] as? String ?? "")")
        }
    }

    @Test func theDefaultKeepsTheChoiceWhileItApplies() throws {
        for v in try Self.list("defaults") {
            let available = try (v["available"] as? [String] ?? []).map { try Self.kind($0) }
            let current = try (v["current"] as? String).map { try Self.kind($0) }
            let want = try (v["kind"] as? String).map { try Self.kind($0) }
            #expect(ExportPanelRules.defaultKind(available, keeping: current) == want)
        }
    }

    @Test func optionsShowOnlyWhereTheyApply() throws {
        for v in try Self.list("applies") {
            let kind = try Self.kind(v["kind"])
            let editable = v["editable"] as? Bool ?? false
            #expect(ExportPanelRules.usesCameras(kind) == (v["cameras"] as? Bool), "cameras for \(kind)")
            #expect(ExportPanelRules.usesSound(kind) == (v["sound"] as? Bool), "sound for \(kind)")
            #expect(ExportPanelRules.usesQuality(kind, editable: editable) == (v["quality"] as? Bool), "quality for \(kind)")
        }
    }

    @Test func problemsStopAnExportBeforeItStarts() throws {
        for v in try Self.list("problems") {
            let got = ExportPanelRules.problem(
                kind: try Self.kind(v["kind"]), start: Self.int(v["start"]) ?? 0, end: Self.int(v["end"]) ?? 0,
                checkedCameras: Int(Self.int(v["cameras"]) ?? 0), busy: v["busy"] as? Bool ?? false)
            #expect(got == v["problem"] as? String, "\(v)")
        }
    }

    @Test func theSummaryNamesCamerasDurationAndSize() throws {
        for v in try Self.list("summaries") {
            let got = ExportPanelRules.summary(
                kind: try Self.kind(v["kind"]), durationMs: Self.int(v["durationMs"]) ?? 0,
                checkedCameras: Int(Self.int(v["cameras"]) ?? 0), estimatedBytes: Self.int(v["bytes"]))
            #expect(got == v["text"] as? String)
        }
    }

    @Test func theSizeIsEstimatedFromTheSpansTheRangeCovers() throws {
        for v in try Self.list("estimates") {
            let spans = (v["spans"] as? [[NSNumber]] ?? []).map { (start: $0[0].int64Value, end: $0[1].int64Value, bytes: $0[2].int64Value) }
            let got = ExportPanelRules.estimate(spans: spans, start: Self.int(v["start"]) ?? 0, end: Self.int(v["end"]) ?? 0)
            #expect(got == Self.int(v["bytes"]))
        }
    }

    @Test func timesAreParsedAndShownOnTheDay() throws {
        for v in try Self.list("times") {
            let got = ExportPanelRules.parseTime(v["text"] as? String ?? "", dayStartMs: Self.int(v["dayStartMs"]) ?? 0)
            #expect(got == Self.int(v["ms"]), "\(v["text"] as? String ?? "")")
        }
        for v in try Self.list("timeText") {
            #expect(ExportPanelRules.timeText(ms: Self.int(v["ms"]) ?? 0) == v["text"] as? String)
        }
    }

    @Test func qualityIsTheRequestsTier() throws {
        for v in try Self.list("quality") {
            let name = try #require(v["quality"] as? String)
            let q = try #require(ExportQuality(rawValue: name))
            #expect(q.tier == v["tier"] as? String)
        }
        let camera = RecordingCamera(device: "dev", channel: 1)
        let sub = ExportRequest.range(startMs: 1000, endMs: 5000, cameras: [camera], name: nil, options: .init(), quality: .sub)
        #expect(sub.contains(#""tier":"sub""#) && !sub.contains(#""tier":"main""#))
        let main = ExportRequest.range(startMs: 1000, endMs: 5000, cameras: [camera], name: nil, options: .init())
        #expect(main.contains(#""tier":"main""#))
    }

    @Test func theCheckedCamerasDefaultToTheAvailableOnes() {
        #expect(ExportPanelRules.checked(chosen: nil as Set<Int>?, available: [1, 2, 3]) == [1, 2, 3])
        #expect(ExportPanelRules.checked(chosen: [2, 9], available: [1, 2, 3]) == [2])
    }
}
