import CryptoKit
import Foundation
import NVRCore
import Testing

@testable import AlertHost

// The settings document built for the core, and the model fetched on first use.

@Suite struct SettingsDocumentTests {
    private func object(_ json: String) throws -> [String: Any] {
        try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }

    private let stored = Data(
        #"""
        {"enabled":true,
         "cameras":[{"camera":{"device_key":"k-a","channel":0}},{"camera":{"device_key":"k-gone","channel":1}}],
         "sources":[{"key":"sensor-1","cameras":[{"camera":{"device_key":"k-a","channel":2},"weight":1.0}]}],
         "inputs":[{"kind":"mqtt","id":"sensor-1","username":"example-user","topic":"example/topic"}],
         "outputs":[{"kind":"desktop"},{"kind":"home_assistant","id":"ha","endpoint":"http://192.0.2.10:8123"}],
         "secrets":{"sensor-1":"from-the-file"}}
        """#.utf8)

    @Test func deviceKeysBecomeThisRunsIDsAndUnknownDevicesAreLeftOut() throws {
        let built = try AlertSettingsDocument.build(
            stored: stored, deviceID: { $0 == "k-a" ? 7 : nil }, utcOffsetMinutes: -300, secrets: MemorySecrets())
        let doc = try object(built.json)
        let cameras = try #require(doc["cameras"] as? [[String: Any]])
        #expect(cameras.count == 1)
        #expect((cameras[0]["camera"] as? [String: Any])?["device"] as? Int == 7)
        #expect(!built.json.contains("device_key"))
        #expect(built.skipped == 1)
        #expect(doc["utc_offset_min"] as? Int == -300)
        #expect(built.enabled)
    }

    @Test func secretsComeFromTheStoreNeverTheFile() throws {
        let secrets = MemorySecrets(["sensor-1": "mqtt-password-example", "ha": "token-example", "unused": "x"])
        let built = try AlertSettingsDocument.build(
            stored: stored, deviceID: { _ in 1 }, utcOffsetMinutes: 0, secrets: secrets)
        let found = try #require(try object(built.json)["secrets"] as? [String: String])
        #expect(found == ["sensor-1": "mqtt-password-example", "ha": "token-example"])
        #expect(!built.json.contains("from-the-file"))
        // Without credentials there is no secrets member at all.
        let none = try AlertSettingsDocument.build(
            stored: stored, deviceID: { _ in 1 }, utcOffsetMinutes: 0, secrets: MemorySecrets())
        #expect(try object(none.json)["secrets"] == nil)
    }

    @Test func offUnlessEnabledAndNotAnObjectIsRefused() throws {
        let off = try AlertSettingsDocument.build(
            stored: Data("{}".utf8), deviceID: { _ in nil }, utcOffsetMinutes: 0, secrets: MemorySecrets())
        #expect(!off.enabled)
        #expect(throws: AlertSettingsError.notAnObject) {
            try AlertSettingsDocument.build(
                stored: Data("[1]".utf8), deviceID: { _ in nil }, utcOffsetMinutes: 0, secrets: MemorySecrets())
        }
    }

    @Test func desktopOutputFollowsTheDocument() {
        #expect(DesktopOutput(document: "{}").accepts(severity: "normal"))
        #expect(!DesktopOutput(document: "{}").accepts(severity: "low"))
        let high = DesktopOutput(document: #"{"outputs":[{"kind":"desktop","min_severity":"high"}]}"#)
        #expect(!high.accepts(severity: "normal") && high.accepts(severity: "critical"))
        #expect(!DesktopOutput(document: #"{"outputs":[{"kind":"home_assistant"}]}"#).accepts(severity: "critical"))
        #expect(!DesktopOutput(document: #"{"outputs":[{"kind":"desktop","enabled":false}]}"#).accepts(severity: "critical"))
    }

    @Test func theAlertEventBecomesATitleAndABody() throws {
        let json = #"""
            {"type":"alert","state":"new","source":"device","decision":{"trigger_id":3,"decision":"alert",
             "severity":"critical","alert_id":9,"class":"person","camera":{"device":2,"channel":1,"lens":0},
             "explanation":"Person on Camera 2 at 22:30 (night)."}}
            """#
        let alert = try #require(AlertSummary(eventJSON: json))
        #expect(alert.alertID == 9 && alert.title == "Critical person alert")
        #expect(alert.camera == AlertCamera(device: 2, channel: 1))
        #expect(alert.explanation.hasPrefix("Person on Camera 2"))
        #expect(AlertSummary(eventJSON: #"{"decision":{"decision":"quiet"}}"#) == nil)
    }

    @Test func theTitleLeadsWithTheCameraName() throws {
        let json = #"""
            {"type":"alert","state":"new","source":"device","camera_name":"Front Door","decision":{"trigger_id":3,
             "decision":"alert","severity":"critical","alert_id":9,"class":"person",
             "camera":{"device":2,"channel":1,"lens":0},"explanation":"Person on Front Door at 22:30."}}
            """#
        let alert = try #require(AlertSummary(eventJSON: json))
        #expect(alert.cameraName == "Front Door")
        #expect(alert.title == "Front Door: Critical person alert")
    }
}

@Suite struct ModelInstallerTests {
    private func temp() -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-model-test-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// An archive of made-up bytes and its pinned manifest.
    private func archive(in dir: URL) throws -> (file: URL, manifest: ModelManifest) {
        let file = dir.appendingPathComponent("model.zip")
        let bytes = Data((0..<4096).map { UInt8($0 % 251) })
        try bytes.write(to: file)
        let sum = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        return (file, ModelManifest(name: "rtdetr_r18vd", bytes: bytes.count, sha256: sum))
    }

    /// Stands in for `ditto`: makes the model folder the archive would hold.
    private static let fakeUnpack: ModelInstaller.Unpack = { _, into in
        let model = into.appendingPathComponent("rtdetr_r18vd.mlmodelc", isDirectory: true)
        try FileManager.default.createDirectory(at: model, withIntermediateDirectories: true)
        try Data("coreml".utf8).write(to: model.appendingPathComponent("model.mil"))
    }

    @Test func archiveAddressesFollowTheUpdateSource() {
        let name = "rtdetr_r18vd"
        #expect(ModelManifest.archiveURL(updateSource: "/Volumes/share/nvrview", name: name)?.path
            == "/Volumes/share/nvrview/models/rtdetr_r18vd.mlmodelc.zip")
        #expect(ModelManifest.archiveURL(updateSource: "https://updates.example.com/nvrview/updates.json", name: name)?.absoluteString
            == "https://updates.example.com/nvrview/models/rtdetr_r18vd.mlmodelc.zip")
        #expect(ModelManifest.archiveURL(updateSource: "http://localhost:8080/nvrview/", name: name) != nil)
        #expect(ModelManifest.archiveURL(updateSource: "http://example.com/nvrview/", name: name) == nil)
        #expect(ModelManifest.archiveURL(updateSource: "https://user:pass@example.com/", name: name) == nil)
        #expect(ModelManifest.archiveURL(updateSource: "ftp://example.com/", name: name) == nil)
        #expect(ModelManifest.archiveURL(updateSource: "  ", name: name) == nil)
    }

    @Test func aPresentModelIsUsedWithoutDownloading() async {
        let present = URL(fileURLWithPath: "/tmp/example.mlmodelc")
        let installer = ModelInstaller(
            manifest: nil, locate: { present }, fetch: { _ in throw URLError(.badURL) })
        #expect(await installer.ensure(updateSource: nil) == .ready(present))
    }

    @Test func noSourceOrNoPublishedModelSaysSo() async {
        let dir = temp()
        let pinned = ModelManifest(name: "rtdetr_r18vd", bytes: 10, sha256: String(repeating: "a", count: 64))
        let none = ModelInstaller(manifest: pinned, folder: dir, locate: { nil }, fetch: { _ in throw URLError(.badURL) })
        guard case .missing(let why) = await none.ensure(updateSource: nil) else { Issue.record("not missing"); return }
        #expect(why.contains("update source"))
        let unpublished = ModelInstaller(
            manifest: ModelManifest(name: "rtdetr_r18vd", bytes: 0, sha256: ""), folder: dir, locate: { nil })
        guard case .missing(let why2) = await unpublished.ensure(updateSource: "/x") else { Issue.record("not missing"); return }
        #expect(why2.contains("published"))
        #expect(!(await none.status.message).isEmpty)
    }

    @Test func aCheckedArchiveIsUnpackedIntoTheModelsFolder() async throws {
        let dir = temp()
        let (file, manifest) = try archive(in: dir)
        let models = dir.appendingPathComponent("Models")
        let fetched = FetchLog()
        let installer = ModelInstaller(
            manifest: manifest, folder: models, locate: { nil },
            fetch: { url in
                fetched.add(url)
                let copy = dir.appendingPathComponent("copy.zip")
                try? FileManager.default.removeItem(at: copy)
                try FileManager.default.copyItem(at: file, to: copy)
                return copy
            }, unpack: Self.fakeUnpack)
        let status = await installer.ensure(updateSource: "https://updates.example.com/nvrview/")
        let url = try #require(status.url)
        #expect(url.lastPathComponent == "rtdetr_r18vd.mlmodelc")
        #expect(FileManager.default.fileExists(atPath: url.appendingPathComponent("model.mil").path))
        #expect(fetched.urls.map(\.absoluteString) == ["https://updates.example.com/nvrview/models/rtdetr_r18vd.mlmodelc.zip"])
        // Nothing but the model is left in the folder.
        #expect(try FileManager.default.contentsOfDirectory(atPath: models.path) == ["rtdetr_r18vd.mlmodelc"])
    }

    @Test func aWrongSizeOrChecksumIsRefusedBeforeUnpacking() async throws {
        let dir = temp()
        let (file, manifest) = try archive(in: dir)
        var wrongSum = manifest
        wrongSum.sha256 = String(repeating: "0", count: 64)
        var wrongSize = manifest
        wrongSize.bytes += 1
        for bad in [wrongSum, wrongSize] {
            let unpacked = FetchLog()
            let installer = ModelInstaller(
                manifest: bad, folder: dir.appendingPathComponent("Models"), locate: { nil },
                fetch: { _ in
                    let copy = dir.appendingPathComponent("c-\(UUID().uuidString).zip")
                    try FileManager.default.copyItem(at: file, to: copy)
                    return copy
                },
                unpack: { _, into in
                    unpacked.add(into)
                    try Self.fakeUnpack(file, into)
                })
            guard case .failed(let why) = await installer.ensure(updateSource: "/srv/nvrview") else {
                Issue.record("a bad archive was accepted")
                return
            }
            #expect(why.contains("differs"))
            #expect(unpacked.urls.isEmpty)
        }
    }

    @Test func aFetchThatFailsIsReportedNotThrown() async throws {
        let dir = temp()
        let (_, manifest) = try archive(in: dir)
        let installer = ModelInstaller(
            manifest: manifest, folder: dir, locate: { nil }, fetch: { _ in throw URLError(.cannotConnectToHost) })
        guard case .failed = await installer.ensure(updateSource: "/srv/nvrview") else {
            Issue.record("a failed download was not reported")
            return
        }
    }

    @Test func theRealUnpackerReadsADittoArchive() async throws {
        let dir = temp()
        let model = dir.appendingPathComponent("src/rtdetr_r18vd.mlmodelc", isDirectory: true)
        try FileManager.default.createDirectory(at: model, withIntermediateDirectories: true)
        try Data("coreml".utf8).write(to: model.appendingPathComponent("model.mil"))
        let zip = dir.appendingPathComponent("model.zip")
        let make = Process()
        make.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        make.arguments = ["-c", "-k", "--keepParent", model.path, zip.path]
        try make.run()
        make.waitUntilExit()
        let bytes = try Data(contentsOf: zip)
        let sum = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        let installer = ModelInstaller(
            manifest: ModelManifest(name: "rtdetr_r18vd", bytes: bytes.count, sha256: sum),
            folder: dir.appendingPathComponent("Models"), locate: { nil })
        let status = await installer.ensure(updateSource: dir.path.hasPrefix("/") ? "\(dir.path)/published" : nil)
        // The archive is not at <source>/models/..., so the fetch fails; the
        // real unpacker is exercised below through a source that has it.
        guard case .failed = status else { Issue.record("expected a failed fetch, got \(status)"); return }
        let published = dir.appendingPathComponent("published/models", isDirectory: true)
        try FileManager.default.createDirectory(at: published, withIntermediateDirectories: true)
        try FileManager.default.copyItem(at: zip, to: published.appendingPathComponent("rtdetr_r18vd.mlmodelc.zip"))
        let again = await installer.ensure(updateSource: "\(dir.path)/published")
        let url = try #require(again.url, "\(again)")
        #expect(FileManager.default.fileExists(atPath: url.appendingPathComponent("model.mil").path))
    }
}

private final class FetchLog: @unchecked Sendable {
    private let lock = NSLock()
    private var list: [URL] = []
    func add(_ url: URL) { lock.withLock { list.append(url) } }
    var urls: [URL] { lock.withLock { list } }
}
