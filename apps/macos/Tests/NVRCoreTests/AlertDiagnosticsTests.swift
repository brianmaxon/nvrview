import CoreGraphics
import FakeCore
import Foundation
import NVRCore
import Testing

// Debugging and reviewing smart alerts (minor 14; docs/NOTIFICATIONS.md
// "Diagnostics" and "Evidence"): connection lines, the message log, the Events
// window's filters, the evidence model (parsing, overlay mapping, agreement,
// the comparison summary) and the fake core's scripted versions of the calls.

@Suite struct SensorDiagnosticsTests {
    private func status(_ s: String) -> JSONValue { JSONValue.parse(s)! }
    private let zone = TimeZone(identifier: "UTC")!
    private let locale = Locale(identifier: "en_US_POSIX")
    /// The time formats separate the time and AM/PM with a narrow space.
    private func plain(_ s: String) -> String { s.replacingOccurrences(of: "\u{202F}", with: " ") }

    @Test func aFailedConnectionNamesItsReasonAndLastSuccess() throws {
        let doc = status(
            #"""
            {"input_status":[{"index":0,"id":"sensor-1","kind":"mqtt","status":{"status":"backoff",
             "last_error":"the broker refused the connection (nothing is listening on that port)",
             "last_error_kind":"refused","last_error_ms":1718900100000,"last_success_ms":1718900000000,
             "messages":4,"triggers":1}}]}
            """#)
        let c = try #require(SensorConnection.find(id: "sensor-1", in: doc))
        #expect(c.errorKind == "refused" && c.lastSuccessMs == 1_718_900_000_000)
        let line = c.line(now: Date(timeIntervalSince1970: 1_718_900_200), timeZone: zone, locale: locale)
        #expect(line.hasPrefix("Connection refused: the broker refused the connection"))
        #expect(plain(line).contains("Last connected 4:13:20 PM"), "\(line)")
        #expect(SensorConnection.find(id: "other", in: doc) == nil)
    }

    @Test func neverConnectedLoginRejectedAndConnected() {
        let never = SensorConnection(
            state: "backoff", lastError: "the broker's name did not resolve", errorKind: "dns")
        #expect(never.line() == "Name lookup failed: the broker's name did not resolve. Never connected")
        let login = SensorConnection(state: "auth_failed", lastError: "the broker rejected the login", errorKind: "login")
        #expect(login.line().contains("Login rejected: the broker rejected the login. not retried"))
        #expect(SensorConnection(state: "connected", messages: 1).line() == "Connected (1 message so far)")
        #expect(SensorConnection(state: "connecting").line() == "Connecting…")
        for kind in ["tls_certificate", "tls_hostname", "timeout", "protocol", "not_local", "network"] {
            #expect(!SensorConnection.kindTitle(kind).isEmpty && !SensorConnection.kindTitle(kind).contains("_"))
        }
    }

    private func page(_ range: Range<Int64>, ignoring: Set<Int64> = []) -> SensorMessagePage {
        SensorMessagePage(
            entries: range.map { n in
                ignoring.contains(n)
                    ? SensorMessage(
                        seq: n, atMs: n, topic: "t", payload: "x", outcomeCode: "not_a_trigger_value",
                        outcomeText: "ignored: payload not a trigger value")
                    : SensorMessage(
                        seq: n, atMs: n, topic: "t", payload: "ON", outcomeCode: "matched",
                        outcomeText: "matched sensor s as sensor_motion (start)")
            }, nextSeq: range.upperBound, total: range.count)
    }

    @Test func theLogMergesPagesFiltersByOutcomeAndKeeps500() {
        var log = SensorMessageLog()
        log.merge(page(0..<3, ignoring: [1]))
        log.merge(page(2..<5))
        #expect(log.entries.map(\.seq) == [0, 1, 2, 3, 4] && log.nextSeq == 5)
        #expect(log.shown(.all).map(\.seq) == [4, 3, 2, 1, 0], "newest first")
        #expect(log.shown(.matched).count == 4 && log.shown(.ignored).map(\.seq) == [1])
        #expect(log.shown(.code("not_a_trigger_value")).count == 1 && log.shown(.code("rate_limited")).isEmpty)
        log.merge(page(5..<600))
        #expect(log.entries.count == 500 && log.entries.first?.seq == 100 && log.nextSeq == 600)
        log.clear()
        #expect(log.entries.isEmpty && log.nextSeq == 600)
    }

    @Test func aPageParsesFromTheCoresJSON() throws {
        let text = #"""
            {"entries":[{"seq":7,"at_ms":5,"topic":"example/topic","payload":"hex: ff 00","payload_len":2,
             "truncated":false,"binary":true,"retained":false,
             "outcome":{"code":"not_a_trigger_value","text":"ignored: payload not a trigger value"}}],
             "next_seq":8,"total":1,"more":false}
            """#
        let p = try #require(SensorMessagePage.parse(text))
        #expect(p.nextSeq == 8 && p.entries[0].binary && p.entries[0].outcomeCode == "not_a_trigger_value")
        #expect(
            SensorMessagePage.request(input: "s", after: 3, limit: 10, clear: true)
                == #"{"after":3,"clear":true,"input":"s","limit":10}"#)
    }

    @Test func theTargetListSeparatesEntitiesFromServices() throws {
        let json = #"""
            {"type":"alerts_output_targets","output":1,"ok":true,"summary":"","targets":[
            {"name":"example_phone","kind":"entity"},{"name":"example_old","kind":"service"}]}
            """#
        let list = try #require(NotifyTargetList(json: json))
        #expect(list.ok && list.entities.map(\.name) == ["example_phone"] && list.services.map(\.name) == ["example_old"])
        let failed = try #require(
            NotifyTargetList(json: #"{"output":1,"ok":false,"summary":"token rejected (401)","targets":[]}"#))
        #expect(!failed.ok && failed.summary == "token rejected (401)" && failed.targets.isEmpty)
        if case .outputTargets = SmartAlertEvent.parse(Data(json.utf8)) {} else { Issue.record("not parsed as an event") }
    }

    @Test func aSuccessfulTestSaysHowItWasSent() throws {
        let ok = try #require(
            OutputTestResult(
                json: #"{"output":1,"ok":true,"http_status":200,"summary":"Home Assistant: notify.example_phone sent to the notify entity (200)","at_ms":0}"#))
        #expect(
            plain(ok.line(timeZone: zone, locale: locale))
                == "Delivered (HTTP 200) at 12:00:00 AM: Home Assistant: notify.example_phone sent to the notify entity (200)")
    }

    @Test func aTestResultReadsAsDeliveredOrNot() throws {
        let ok = try #require(
            OutputTestResult(json: #"{"type":"alerts_output_test","output":1,"ok":true,"http_status":200,"at_ms":0}"#))
        #expect(plain(ok.line(timeZone: zone, locale: locale)) == "Delivered (HTTP 200) at 12:00:00 AM")
        let bad = try #require(
            OutputTestResult(
                json: #"{"output":1,"ok":false,"http_status":404,"summary":"entity not found","at_ms":0}"#))
        #expect(
            plain(bad.line(timeZone: zone, locale: locale))
                == "Not delivered (HTTP 404) at 12:00:00 AM: entity not found")
    }
}

@Suite struct EventsFilterTests {
    private func entry(
        decision: String, reason: String? = nil, test: Bool = false, sensor: String? = nil, source: String = "device"
    ) -> SmartAlertLogEntry {
        var d: [String: Any] = [
            "trigger_id": 1, "decision": decision, "decided_ms": 1000, "class": "person", "explanation": "x.",
        ]
        if let reason { d["reason"] = reason }
        var e: [String: Any] = ["logged_ms": 1000, "source": source, "decision": d]
        if test { e["test"] = true }
        if let sensor { e["sensor"] = sensor }
        let data = try! JSONSerialization.data(withJSONObject: e)
        return SmartAlertLogEntry(JSONValue.parse(data)!)!
    }

    @Test func categoriesSeparateNotConfirmedQuietAndTests() {
        #expect(entry(decision: "alert").category == "alert")
        #expect(entry(decision: "quiet", reason: "not_confirmed").category == "not_confirmed")
        #expect(entry(decision: "quiet", reason: "motion_only").category == "quiet")
        #expect(entry(decision: "alert", test: true).category == "test")
        #expect(entry(decision: "suppress", reason: "muted").category == "suppress")
    }

    @Test func theFilterMatchesCategoriesAndSensors() {
        var f = SmartAlertLogFilter()
        let all = [
            entry(decision: "alert"), entry(decision: "quiet", reason: "not_confirmed"),
            entry(decision: "quiet", reason: "motion_only"), entry(decision: "alert", test: true, sensor: "s1", source: "external"),
        ]
        f.decisions = ["not_confirmed"]
        #expect(all.filter(f.matches).count == 1)
        f.decisions = ["quiet", "not_confirmed"]
        #expect(all.filter(f.matches).count == 2)
        f.decisions = ["test"]
        #expect(all.filter(f.matches).count == 1)
        f.decisions = []
        f.sensor = "s1"
        #expect(all.filter(f.matches).count == 1)
        f.sensor = "s2"
        #expect(all.filter(f.matches).isEmpty)
    }

    @Test func theQueryAsksTheCoreForWhatItCanFilter() throws {
        func query(_ f: SmartAlertLogFilter) throws -> [String: Any] {
            try #require(JSONSerialization.jsonObject(with: Data(f.query().utf8)) as? [String: Any])
        }
        var f = SmartAlertLogFilter()
        f.decisions = ["not_confirmed", "alert"]
        f.sensor = "s1"
        let q = try query(f)
        #expect(q["decision"] as? [String] == ["alert", "quiet"] && q["sensor"] as? String == "s1")
        f.decisions = ["test", "alert"]
        #expect(try query(f)["decision"] == nil, "a test can have any decision")
    }
}

@Suite struct EvidenceModelTests {
    private let json = #"""
        {"trigger_id":9,"created_ms":1000,"expires_ms":7201000,"pinned":false,"bytes":10,
         "frames":[{"index":1,"at_ms":2,"bytes":5},{"index":0,"at_ms":1,"bytes":5}],
         "trigger":{"source":"device","kind":"people","class":"person"},
         "decision":{"decision":"quiet","reason":"not_confirmed","trigger_id":9},
         "methods":{"detector":{"frames":{"0":{"boxes":[
            {"label":"car","score":0.9,"box":{"x":0.1,"y":0.2,"w":0.3,"h":0.4}},
            {"label":"person","score":0.3,"box":{"x":0.5,"y":0.5,"w":0.1,"h":0.2}}]}}},
          "other_verifier":{"claimed":"person"}}}
        """#

    private func near(_ a: CGRect, _ b: CGRect) -> Bool {
        abs(a.minX - b.minX) < 0.01 && abs(a.minY - b.minY) < 0.01 && abs(a.width - b.width) < 0.01
            && abs(a.height - b.height) < 0.01
    }

    private func evidence(_ text: String? = nil) throws -> AlertEvidence {
        let parsed = try #require(JSONValue.parse(text ?? json))
        return try #require(AlertEvidence(parsed))
    }

    @Test func parsesFramesMethodsAndTheCamerasClaim() throws {
        let e = try evidence()
        #expect(e.frames.map(\.index) == [0, 1], "sorted")
        #expect(e.methods.map(\.name) == ["detector", "other_verifier"], "ours before the others")
        #expect(e.cameraClaim == "person", "the trigger's class when a camera raised it")
        #expect(e.method("detector")?.boxes[0]?.count == 2)
        #expect(e.detectorClasses == ["vehicle"], "the 0.3 person is below the 0.5 floor")
        #expect(e.decision == "quiet" && !e.isTest && !e.pinned)
    }

    @Test func theMethodsDisagreeWhenTheDetectorMissedTheClaim() throws {
        let e = try evidence()
        #expect(e.agreement == .disagree && e.methodsDisagree)
        let agreed = try evidence(json.replacingOccurrences(of: #""score":0.3"#, with: #""score":0.8"#))
        #expect(agreed.agreement == .agree && !agreed.methodsDisagree)
        let sensor = try evidence(json.replacingOccurrences(of: #""source":"device""#, with: #""source":"external""#))
        #expect(sensor.agreement == .notApplicable)
    }

    @Test func boxesLandInsideTheAspectFitPicture() throws {
        let view = CGSize(width: 400, height: 400)
        // A 2:1 picture in a square view: 400 x 200 centered vertically.
        let picture = CGSize(width: 1280, height: 640)
        #expect(EvidenceOverlay.pictureRect(picture: picture, in: view) == CGRect(x: 0, y: 100, width: 400, height: 200))
        let e = try evidence()
        let drawn = EvidenceOverlay.layout(e, frame: 0, visible: ["detector"], picture: picture, in: view)
        #expect(drawn.count == 2 && drawn[0].colorIndex == 0 && drawn[0].method == "detector")
        #expect(near(drawn[0].rect, CGRect(x: 40, y: 140, width: 120, height: 80)))
        #expect(drawn[0].caption == "car 90%")
        // Toggling a method off removes its boxes; another frame has none.
        #expect(EvidenceOverlay.layout(e, frame: 0, visible: [], picture: picture, in: view).isEmpty)
        #expect(EvidenceOverlay.layout(e, frame: 1, visible: ["detector"], picture: picture, in: view).isEmpty)
        // A box past the edge is clamped to the picture.
        let wide = EvidenceBox(label: "person", score: 1, x: 0.9, y: -0.5, w: 0.5, h: 1)
        #expect(
            near(EvidenceOverlay.rect(for: wide, picture: picture, in: view), CGRect(x: 360, y: 100, width: 40, height: 100)))
    }

    @Test func theSummaryCountsAgreementsPerClass() throws {
        func set(claim: String, found: String?) throws -> AlertEvidence {
            let boxes =
                found.map {
                    #"{"label":"\#($0)","score":0.9,"box":{"x":0,"y":0,"w":0.1,"h":0.1}}"#
                } ?? ""
            return try evidence(
                #"""
                {"trigger_id":1,"trigger":{"source":"device","class":"\#(claim)"},
                 "methods":{"detector":{"frames":{"0":{"boxes":[\#(boxes)]}}}}}
                """#)
        }
        let sets = [
            try set(claim: "person", found: "person"), try set(claim: "person", found: "person"),
            try set(claim: "person", found: nil), try set(claim: "vehicle", found: "person"),
        ]
        let c = EvidenceComparison(sets)
        #expect(c.compared == 4)
        let person = try #require(c.rows.first { $0.cls == "person" })
        #expect((person.agreed, person.cameraOnly, person.detectorOnly) == (2, 1, 1))
        let vehicle = try #require(c.rows.first { $0.cls == "vehicle" })
        #expect((vehicle.agreed, vehicle.cameraOnly, vehicle.detectorOnly) == (0, 1, 0))
        #expect(c.line(person) == "Person: 2 agreed, 1 camera only, 1 detector only")
    }

    @Test func theDocumentTheAppPutsRoundTrips() throws {
        let boxes = [EvidenceBox(label: "person", score: 0.8, x: 0.1, y: 0.2, w: 0.3, h: 0.4)]
        let request = EvidenceDocument.request(
            triggerID: 9, frame: 2, atMs: 5, methods: ["detector": (claimed: nil, boxes: boxes)])
        let o = try #require(JSONValue.parse(request))
        #expect(o["trigger_id"]?.int64 == 9 && o["frame"]?.int64 == 2)
        let methods = try #require(o["doc"]?["methods"])
        let method = try #require(methods["detector"])
        let m = try #require(EvidenceMethod(name: "detector", method))
        #expect(m.boxes[2] == boxes)
    }
}

@Suite struct FakeDiagnosticsTests {
    @Test func theFakeCoreScriptsMessagesTestsAndEvidence() throws {
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.recordingWorkIsManual = true
        let rig = try Rig(options)
        defer { rig.bridge.shutdown() }
        let core = try rig.bridge.smartAlerts()
        try core.alertsConfigure(
            json: #"{"inputs":[{"kind":"mqtt","id":"sensor-1"}],"outputs":[{"kind":"desktop"},{"kind":"home_assistant"}]}"#)

        // Messages: unknown input, paging, clearing.
        #expect(throws: CoreStatus.unknownID) { try core.alertsMessages(request: #"{"input":"nope"}"#) }
        for n in 0..<3 {
            rig.fake.logSensorMessage(
                input: "sensor-1", topic: "t", payload: n == 1 ? "x" : "ON", outcomeCode: n == 1 ? "not_a_trigger_value" : "matched",
                outcomeText: "o")
        }
        let page = try #require(SensorMessagePage.parse(try core.alertsMessages(request: SensorMessagePage.request(input: "sensor-1", after: 0))))
        #expect(page.entries.map(\.seq) == [0, 1, 2] && page.nextSeq == 3)
        let rest = try #require(SensorMessagePage.parse(try core.alertsMessages(request: SensorMessagePage.request(input: "sensor-1", after: 3))))
        #expect(rest.entries.isEmpty)
        let cleared = try #require(
            SensorMessagePage.parse(try core.alertsMessages(request: SensorMessagePage.request(input: "sensor-1", after: 0, clear: true))))
        #expect(cleared.total == 0)

        // A simulated trigger is a test in the log and the events; evidence is kept with its decision.
        let camera = AlertCamera(device: 1, channel: 0)
        let id = try core.alertsTriggerPut(
            json: #"{"source":"device","camera":\#(camera.json),"kind":"people","test":true}"#)
        #expect(rig.fake.testSentToHomeAssistant(trigger: id) == false)
        let jpeg = Data([0xFF, 0xD8, 1, 0xFF, 0xD9])
        let put = EvidenceDocument.request(
            triggerID: id, frame: 0, atMs: 1,
            methods: ["detector": (claimed: nil, boxes: [EvidenceBox(label: "person", score: 0.9, x: 0, y: 0, w: 0.5, h: 0.5)])])
        try core.alertEvidencePut(json: put, data: jpeg)
        #expect(throws: CoreStatus.unknownID) { try core.alertEvidencePut(json: #"{"trigger_id":999,"doc":{}}"#, data: nil) }
        rig.fake.expireAlertRequests()
        rig.fake.completeAlertWork()
        let log = try #require(SmartAlertLogPage.parse(try core.alertsGet(query: #"{"test":true}"#)))
        #expect(log.entries.count == 1 && log.entries[0].isTest && log.entries[0].category == "test")
        let setJSON = try #require(JSONValue.parse(try core.alertEvidenceGet(query: #"{"trigger_id":\#(id)}"#)))
        let set = try #require(AlertEvidence(setJSON))
        #expect(set.decision != nil && set.method("detector")?.boxes[0]?.count == 1 && set.isTest)
        #expect(try core.alertEvidenceFrameGet(triggerID: id, index: 0) == jpeg)
        try core.alertEvidencePin(triggerID: id)
        let list = try #require(EvidenceList.parse(try core.alertEvidenceGet(query: nil)))
        #expect(list.sets.map(\.triggerID) == [id] && list.sets[0].pinned)
        #expect(throws: CoreStatus.unknownID) { try core.alertEvidencePin(triggerID: 999) }

        // Send test alert: the scripted result arrives as an event.
        rig.fake.setOutputTestResult(ok: false, httpStatus: 404, summary: "entity not found")
        try core.alertsOutputTest(request: #"{"output":1}"#)
        #expect(throws: CoreStatus.unknownID) { try core.alertsOutputTest(request: #"{"output":5}"#) }
        rig.fake.completeAlertWork()
        let results: [OutputTestResult] = rig.events.get().compactMap {
            if case .smartAlert(.outputTest(let j)) = $0 { OutputTestResult(json: j) } else { nil }
        }
        #expect(results.count == 1 && !results[0].ok && results[0].httpStatus == 404)

        // The target list for the settings menu arrives the same way.
        rig.fake.setOutputTargets(
            ok: true, summary: "Home Assistant: 2 notify targets",
            targets: [(name: "example_phone", kind: "entity"), (name: "example_old", kind: "service")])
        try core.alertsOutputTargets(request: #"{"output":1}"#)
        #expect(throws: CoreStatus.unknownID) { try core.alertsOutputTargets(request: #"{"output":5}"#) }
        rig.fake.completeAlertWork()
        let lists: [NotifyTargetList] = rig.events.get().compactMap {
            if case .smartAlert(.outputTargets(let j)) = $0 { NotifyTargetList(json: j) } else { nil }
        }
        #expect(lists.count == 1 && lists[0].entities.count == 1 && lists[0].services.count == 1)
    }
}
