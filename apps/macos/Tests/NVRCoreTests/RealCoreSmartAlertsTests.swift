#if NVRVIEW_REAL_CORE
import Foundation
import NVRCore
import NVRCoreC
import Testing

/// Smart alerts (minor 12) against the real core and a spawned mock-nvr
/// (needs `cargo build -p mock-nvr` first): a trigger from the app wakes a
/// camera, its pictures are fetched through CCore and answered with a
/// person, and the alert reaches the log with a snapshot.
@Suite(.serialized) struct RealCoreSmartAlertsTests {
    private func spawnMock() throws -> (Process, String, UInt16) {
        let mock = Fixtures.root.appendingPathComponent("target/debug/mock-nvr")
        try #require(FileManager.default.isExecutableFile(atPath: mock.path), "build mock-nvr first")
        let p = Process()
        p.executableURL = mock
        p.arguments = [
            "--port", "0", "--channels", "1",
            "--fixture", Fixtures.directory.appendingPathComponent("h264-basic.h264").path,
        ]
        p.environment = ["MOCK_NVR_PASSWORD": "example-password"]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        try p.run()
        var text = ""
        let deadline = Date().addingTimeInterval(5)
        while !text.contains("\n"), Date() < deadline {
            text += String(decoding: out.fileHandleForReading.availableData, as: UTF8.self)
        }
        let line = try #require(text.split(separator: "\n").first { $0.hasPrefix("mock-nvr listening on ") })
        let addr = line.dropFirst("mock-nvr listening on ".count)
        let colon = try #require(addr.lastIndex(of: ":"))
        return (p, String(addr[..<colon]), try #require(UInt16(addr[addr.index(after: colon)...])))
    }

    @Test func aTriggerIsVerifiedAndLogged() throws {
        let (mock, host, port) = try spawnMock()
        defer { mock.terminate() }
        let events = Box<[CoreEvent]>([])
        let bridge = try CoreBridge(factory: CCore.factory(), onEvent: { e in events.update { $0.append(e) } })
        defer { bridge.shutdown() }
        let device = try bridge.addDevice(host: host, port: port, user: "example-user", password: "example-password")
        let core = try bridge.smartAlerts()
        let camera = AlertCamera(device: device, channel: 0)
        try core.alertsConfigure(
            json: #"{"cameras":[{"camera":\#(camera.json)}],"utc_offset_min":0,"acquisition":{"recheck":false}}"#)
        let id = try core.alertsTriggerPut(json: #"{"source":"device","camera":\#(camera.json),"kind":"people"}"#)

        var answered = Set<String>()
        var alertJSON: String?
        let deadline = Date().addingTimeInterval(20)
        while alertJSON == nil, Date() < deadline {
            for e in events.get() {
                guard case .smartAlert(let s) = e else { continue }
                switch s {
                case .verifyRequest(let r) where !answered.contains(r.request):
                    answered.insert(r.request)
                    let frames = try core.alertFramesGet(request: r.request)
                    #expect(frames.kind == .h264Keyframe && frames.data.count == r.bytes)
                    let person = AlertDetection(label: "person", score: 0.9, x: 0.4, y: 0.2, w: 0.2, h: 0.5)
                    try core.alertDetectionsPut(request: r.request, json: AlertDetection.json([person]))
                case .alert("new", let json) where json.contains(#""trigger_id":\#(id)"#):
                    alertJSON = json
                default:
                    break
                }
            }
            Thread.sleep(forTimeInterval: 0.02)
        }
        let json = try #require(alertJSON)
        let o = try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        let alertID = try #require((o["decision"] as? [String: Any])?["alert_id"] as? UInt64)
        try core.alertSnapshotPut(alertID: alertID, data: Data([0xFF, 0xD8, 0xFF, 0xD9]))
        #expect(try core.alertSnapshotGet(alertID: alertID) == Data([0xFF, 0xD8, 0xFF, 0xD9]))
        #expect(try core.alertsGet(query: #"{"alert_id":\#(alertID)}"#).contains(#""snapshot":true"#))
        #expect(throws: CoreStatus.unknownID) { try core.alertFramesGet(request: "v999999") }
    }
}
#endif
