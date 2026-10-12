import FakeCore
import Foundation
import NVRCore
import NVRViewCore
import Testing

// Smart alerts (minor 12; docs/FFI.md "Smart alerts"): the events as the
// core writes them, the detector's answer, and the fake core's scripted
// verifications from a trigger to a logged alert.

@Suite struct SmartAlertContractTests {
    @Test func theVerifyRequestEvent() throws {
        let json = #"""
            {"type":"verify_request","request":"v3","purpose":"check","camera":{"device":1,"channel":2,"lens":0},
             "trigger_ids":[12],"codec":"h264","width":1280,"height":720,"pts_us":2000000,"bytes":5120,
             "frames":[{"pts_us":1000000,"bytes":5000,"keyframe":true},{"pts_us":2000000,"bytes":120,"keyframe":false}],
             "from_cover":false,"at_ms":1718900000250,"deadline_ms":1718900005250,"classes":["person"],
             "min_score":0.5,"crops":[{"what":"frame","x":0,"y":0,"w":1,"h":1},
             {"what":"tile","x":0.45,"y":0,"w":0.55,"h":0.55}],"future":1}
            """#
        guard case .smartAlert(.verifyRequest(let r))? = CoreEvent.parse(Data(json.utf8)) else {
            Issue.record("not a verify_request")
            return
        }
        #expect(r.request == "v3" && r.camera == AlertCamera(device: 1, channel: 2) && r.triggerIDs == [12])
        #expect(r.frames.count == 2 && r.frames[0].keyframe && r.frames.map(\.bytes).reduce(0, +) == r.bytes)
        #expect(r.crops.map(\.what) == ["frame", "tile"] && r.minScore == 0.5 && r.classes == ["person"])
        #expect(NV_ABI_MINOR >= 12)
    }

    @Test func stepsDecisionsAndOthers() {
        let step = #"{"type":"verify_step","trigger_id":12,"step":{"at_ms":1,"step":"gate","result":"pass"}}"#
        #expect(CoreEvent.parse(Data(step.utf8)) == .smartAlert(.verifyStep(triggerID: 12, json: step)))
        let alert = #"{"type":"alert","state":"settled","alert_id":4,"rev":2}"#
        #expect(CoreEvent.parse(Data(alert.utf8)) == .smartAlert(.alert(state: "settled", json: alert)))
        let done = #"{"type":"verify_request_done","request":"v3","result":"timeout"}"#
        #expect(CoreEvent.parse(Data(done.utf8)) == .smartAlert(.verifyRequestDone(request: "v3", result: "timeout")))
        let rejected = #"{"type":"trigger_rejected","source":"external","reason":"rate_limit"}"#
        #expect(CoreEvent.parse(Data(rejected.utf8)) == .smartAlert(.triggerRejected(reason: "rate_limit")))
    }

    @Test func detectionsAreTheContractShape() throws {
        let json = AlertDetection.json([AlertDetection(label: "car", score: 0.75, x: 0.5, y: 0.25, w: 0.125, h: 0.5)])
        let o = try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        let d = try #require((o["detections"] as? [[String: Any]])?.first)
        #expect(d["label"] as? String == "car" && d["score"] as? Double == 0.75)
        #expect((d["box"] as? [String: Double]) == ["x": 0.5, "y": 0.25, "w": 0.125, "h": 0.5])
        #expect(AlertCamera(device: 3, channel: 1).json == #"{"device":3,"channel":1,"lens":0}"#)
    }
}

@Suite struct FakeSmartAlertTests {
    private func smart(_ rig: Rig) -> [SmartAlertEvent] {
        rig.events.get().compactMap { if case .smartAlert(let e) = $0 { e } else { nil } }
    }

    private func requests(_ rig: Rig) -> [VerifyRequest] {
        smart(rig).compactMap { if case .verifyRequest(let r) = $0 { r } else { nil } }
    }

    private func decisions(_ rig: Rig) -> [[String: Any]] {
        smart(rig).compactMap {
            guard case .alert(let state, let json) = $0, state != "settled",
                let o = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]
            else { return nil }
            return o["decision"] as? [String: Any]
        }
    }

    @Test func aVerifiedPersonAlertsAndIsLogged() throws {
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.recordingWorkIsManual = true
        let rig = try Rig(options)
        defer { rig.bridge.shutdown() }
        let core = try rig.bridge.smartAlerts()
        let camera = AlertCamera(device: 1, channel: 0)
        let trigger = #"{"source":"device","camera":\#(camera.json),"kind":"people"}"#

        #expect(throws: CoreStatus.notConfigured) { try core.alertsTriggerPut(json: trigger) }
        #expect(throws: CoreStatus.invalidArgument) { try core.alertsConfigure(json: "{") }
        try core.alertsConfigure(json: "{}")
        let id = try core.alertsTriggerPut(json: trigger)
        rig.fake.completeAlertWork()
        let reqs = requests(rig)
        #expect(reqs.count == 2 && reqs.allSatisfy { $0.triggerIDs == [id] && $0.camera == camera })
        for r in reqs {
            let frames = try core.alertFramesGet(request: r.request)
            #expect(frames.kind == .h264Keyframe && frames.data.starts(with: [0, 0, 0, 1]))
            let person = AlertDetection(label: "person", score: 0.8, x: 0.4, y: 0.3, w: 0.1, h: 0.4)
            try core.alertDetectionsPut(request: r.request, json: AlertDetection.json([person]))
            #expect(throws: CoreStatus.unknownID) {
                try core.alertDetectionsPut(request: r.request, json: AlertDetection.json([]))
            }
        }
        rig.fake.completeAlertWork()
        let d = try #require(decisions(rig).last)
        #expect(d["decision"] as? String == "alert" && d["trigger_id"] as? UInt64 == id)
        let alertID = try #require(d["alert_id"] as? UInt64)

        let jpeg = Data([0xFF, 0xD8, 1, 2, 0xFF, 0xD9])
        try core.alertSnapshotPut(alertID: alertID, data: jpeg)
        #expect(try core.alertSnapshotGet(alertID: alertID) == jpeg)
        #expect(throws: CoreStatus.unknownID) { try core.alertSnapshotPut(alertID: alertID + 9, data: jpeg) }
        try core.alertFeedback(json: #"{"alert_id":\#(alertID),"vote":"down","reason":"nothing_there"}"#)
        #expect(throws: CoreStatus.invalidArgument) {
            try core.alertFeedback(json: #"{"alert_id":\#(alertID),"vote":"up","reason":"duplicate"}"#)
        }
        let log = try core.alertsGet(query: #"{"decision":["alert"]}"#)
        #expect(log.contains(#""snapshot":true"#) && log.contains(#""nothing_there""#))

        // Muted: suppressed, no pictures asked for.
        try core.alertMute(cameraJSON: camera.json, seconds: 600)
        _ = try core.alertsTriggerPut(json: trigger)
        rig.fake.completeAlertWork()
        #expect(decisions(rig).last?["reason"] as? String == "muted")
        #expect(requests(rig).count == 2)
    }

    @Test func noAnswerFailsOpenOnlyForAlwaysAlertCases() throws {
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.recordingWorkIsManual = true
        let rig = try Rig(options)
        defer { rig.bridge.shutdown() }
        let core = try rig.bridge.smartAlerts()
        try core.alertsConfigure(json: "{}")
        let outside = AlertCamera(device: 1, channel: 0)
        rig.fake.setAlwaysAlertCameras([outside])
        _ = try core.alertsTriggerPut(json: #"{"source":"device","camera":\#(outside.json),"kind":"people"}"#)
        _ = try core.alertsTriggerPut(
            json: #"{"source":"device","camera":{"device":1,"channel":1},"kind":"people"}"#)
        _ = try core.alertsTriggerPut(json: #"{"source":"local_audio","camera":\#(outside.json),"kind":"smoke_alarm"}"#)
        rig.fake.expireAlertRequests()
        rig.fake.completeAlertWork()
        let d = decisions(rig)
        #expect(d.count == 3)
        #expect(d.contains { $0["class"] as? String == "smoke" && $0["severity"] as? String == "critical" })
        #expect(d.contains { $0["unverified"] as? Bool == true && $0["severity"] as? String == "critical" })
        #expect(d.contains { $0["reason"] as? String == "could_not_look" })
    }
}
