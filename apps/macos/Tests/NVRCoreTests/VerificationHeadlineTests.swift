import Foundation
import NVRCore
import Testing

// The live check's headline, its elapsed time, the confirmed-only pop-up
// rule, and the notification title that leads with the camera's name
// (docs/UX.md "Live checks").

private func json(_ text: String) -> JSONValue { JSONValue.parse(text)! }

private let name: @Sendable (AlertCamera) -> String = { $0.channel == 0 ? "Front Door" : "Camera \($0.channel + 1)" }

private func started(_ trigger: UInt64 = 7, camera: Int = 0, at now: Double = 100) -> VerificationBoard {
    var b = VerificationBoard()
    b.step(
        triggerID: trigger,
        json(#"{"at_ms":1000,"step":"trigger_received","source":"device","kind":"people","class":"person","camera":{"device":1,"channel":\#(camera),"lens":0},"event_ms":1000,"late":false,"age_ms":0}"#),
        now: now)
    return b
}

private func decide(_ b: inout VerificationBoard, _ decision: String, extra: String = "", at now: Double = 110) {
    let tail =
        decision == "alert"
        ? #","alert_id":4,"severity":"high","rev":1"# : #","reason":"not_confirmed""#
    b.alertEvent(
        json(#"{"type":"alert","state":"new","decision":{"trigger_id":7,"decision":"\#(decision)"\#(tail),"explanation":"E."\#(extra)}}"#),
        now: now)
}

@Suite struct VerifyHeadlineTests {
    @Test func theVerdictComesFirst() {
        var b = started()
        #expect(b.check(7)?.verdictHeadline(cameraName: name) == "Checking: person at Front Door")
        decide(&b, "alert")
        #expect(b.check(7)?.verdictHeadline(cameraName: name) == "Confirmed: person at Front Door")

        var n = started()
        decide(&n, "quiet")
        #expect(n.check(7)?.verdictHeadline(cameraName: name) == "Not confirmed: person at Front Door")
        var s = started()
        decide(&s, "suppress")
        #expect(s.check(7)?.verdictHeadline(cameraName: name) == "Not confirmed: person at Front Door")
    }

    @Test func otherEndingsAreNamedForWhatHappened() {
        var cooldown = started()
        decide(&cooldown, "alert", extra: #","quieted":{"why":"cooldown"}"#)
        #expect(cooldown.check(7)?.verdictHeadline(cameraName: name) == "Confirmed: person at Front Door (not notified)")

        var low = started()
        low.alertEvent(
            json(#"{"type":"alert","state":"logged","decision":{"trigger_id":7,"decision":"quiet","reason":"low_severity","explanation":"E."}}"#),
            now: 110)
        #expect(low.check(7)?.verdictHeadline(cameraName: name) == "Confirmed, quiet log: person at Front Door")

        var muted = started()
        muted.alertEvent(
            json(#"{"type":"alert","state":"logged","decision":{"trigger_id":7,"decision":"suppress","reason":"muted","explanation":"E."}}"#),
            now: 110)
        #expect(muted.check(7)?.verdictHeadline(cameraName: name) == "Not alerted: person at Front Door")

        var open = started()
        open.step(triggerID: 7, json(#"{"at_ms":2000,"step":"decision","decision":"alert","severity":"critical","unverified":true}"#), now: 101)
        decide(&open, "alert")
        #expect(open.check(7)?.verdictHeadline(cameraName: name) == "Alert, not confirmed: person at Front Door")
    }

    @Test func aSensorAlarmWithoutACameraLeavesTheCameraOut() {
        var b = VerificationBoard()
        b.step(triggerID: 3, json(#"{"at_ms":1,"step":"trigger_received","source":"external","kind":"smoke","class":"smoke","sensor":0}"#), now: 0)
        #expect(b.check(3)?.verdictHeadline(cameraName: name) == "Checking: smoke")
        b.step(triggerID: 4, json(#"{"at_ms":1,"step":"trigger_received","source":"external","kind":"co","class":"co","sensor":0}"#), now: 0)
        #expect(b.check(4)?.verdictHeadline(cameraName: name) == "Checking: CO")
    }

    @Test func elapsedTimeCountsUp() {
        #expect(ElapsedText.ago(seconds: 0.4) == "just now")
        #expect(ElapsedText.ago(seconds: 12) == "12 s ago")
        #expect(ElapsedText.ago(seconds: 59.9) == "59 s ago")
        #expect(ElapsedText.ago(seconds: 60) == "1 min ago")
        #expect(ElapsedText.ago(seconds: 3 * 60 + 40) == "3 min ago")
        #expect(ElapsedText.ago(seconds: 3599) == "59 min ago")
        #expect(ElapsedText.ago(seconds: 3600) == "1 h ago")
        #expect(ElapsedText.ago(seconds: 2 * 3600 + 1800) == "2 h ago")
        #expect(ElapsedText.ago(seconds: -5) == "just now")
        #expect(ElapsedText.ago(since: 100, now: 112) == "12 s ago")
    }

    @Test func confirmedOnlyPopsUpWhenTheAlertIsConfirmed() {
        var b = started()
        #expect(b.mayPopUp(b.check(7)!))
        b.settings.popUp = .confirmedOnly
        #expect(!b.mayPopUp(b.check(7)!))
        decide(&b, "quiet")
        #expect(!b.mayPopUp(b.check(7)!))
        var a = started()
        a.settings.popUp = .confirmedOnly
        decide(&a, "alert")
        #expect(a.mayPopUp(a.check(7)!))
    }

    @Test func theNewSettingsDefaultAndRoundTrip() throws {
        let old = Data(#"{"placement":"window","showQuiet":true,"minSeconds":30}"#.utf8)
        let v = try JSONDecoder().decode(VerifyViewSettings.self, from: old)
        #expect(v.placement == .window && v.showQuiet && v.minSeconds == 30)
        #expect(v.detail == .fullLog && v.popUp == .always)
        var s = SmartAlertSettings()
        s.view.detail = .headline
        s.view.popUp = .confirmedOnly
        let back = SmartAlertSettings.fromFile(s.fileData())
        #expect(back.view.detail == .headline && back.view.popUp == .confirmedOnly)
    }
}

@Suite struct AlertTitleTests {
    @Test func theTitleLeadsWithTheCameraName() {
        #expect(
            AlertText.title(cameraName: "Front Door", className: "person", severity: "critical", isTest: false)
                == "Front Door: Critical person alert")
        #expect(
            AlertText.title(cameraName: "Camera 3", className: "vehicle", severity: "normal", isTest: false)
                == "Camera 3: Vehicle alert")
        #expect(
            AlertText.title(cameraName: "Front Door", className: "person", severity: "normal", isTest: true)
                == "Front Door: Test person alert")
    }

    @Test func noNameKeepsTheOldTitle() {
        #expect(AlertText.title(cameraName: nil, className: "smoke", severity: "critical", isTest: false) == "Critical smoke alarm")
        #expect(AlertText.title(cameraName: " ", className: "person", severity: "normal", isTest: true) == "Test: Person alert")
    }
}
