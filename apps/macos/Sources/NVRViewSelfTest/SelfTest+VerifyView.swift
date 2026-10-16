@testable import NVRViewApp
import AppKit
import Foundation
import NVRCore
import AppStage

// The live check's headline, elapsed time, and confirmed-only pop-up: the
// self-test's "alerts" steps (scripts/macos-selftest-areas.txt, verify-view-*).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// Called with the scripted check of a camera on screen (its band is up
    /// and it has alerted).
    func verifyHeadlineStep(_ camera: CameraRef, check: VerifyCheck?) {
        guard let model = app.model, let window, let check else { return checkLog() }
        let alerts = model.alertsModel!
        let stage = window.stage
        // parity: verify-view-headline
        // The band's heading says how long ago the check started, counting up
        // on the viewer clock ("12 s ago", then minutes). The "single
        // headline" setting replaces the step log with one line that leads
        // with the verdict and names the camera.
        func heading() -> String { stage.verificationBand(for: camera)?.content?.headline ?? "-" }
        func expected() -> String { ElapsedText.ago(since: check.startedAt, now: model.clock.now) }
        if !heading().contains(expected()) { problems.append("the band's heading lacks \"\(expected())\": \(heading())") }
        advance(30)
        let at30 = heading()
        let words30 = expected()
        advance(100)
        let at130 = heading()
        let words130 = expected()
        print("verify view elapsed: \(at30) | \(at130)")
        if !at30.contains(words30) || !at130.contains(words130) || words30 == words130 {
            problems.append("the elapsed time did not count up: \(at30), then \(at130)")
        }
        if !words130.contains("min ago") { problems.append("minutes did not read in minutes: \(words130)") }
        alerts.update { $0.view.detail = .headline }
        let content = stage.verificationBand(for: camera)?.content
        let want = "Confirmed: person at \(model.name(of: camera)) · \(expected())"
        print("verify view headline: \(content?.headline ?? "-"), lines \(content?.lines.count ?? -1)")
        if content?.headline != want { problems.append("the single headline is \(content?.headline ?? "-"), not \(want)") }
        if content?.lines.isEmpty != true { problems.append("the single headline still shows the step log") }
        alerts.update { $0.view.detail = .fullLog }
        if stage.verificationBand(for: camera)?.content?.lines.isEmpty != false {
            problems.append("the full log did not come back")
        }
    }

    /// With "only when the alert is confirmed": a check of a camera the
    /// window does not show opens no inset while it runs, one when it
    /// confirms (with the verdict headline), and a check that is not
    /// confirmed never opens one. Called with another tile focused.
    func verifyPopUpStep(next: @escaping @MainActor () -> Void) {
        guard let model = app.model, let window else { return checkLog() }
        let alerts = model.alertsModel!
        let stage = window.stage
        let leaves = stage.frames.leaves
        guard leaves.count >= 3, let a = leaves[1].camera, let b = leaves[2].camera,
            let t1 = smartTrigger(a, kind: "people")
        else { return checkLog() }
        // parity: verify-view-popup
        alerts.update {
            $0.view.popUp = .confirmedOnly
            $0.view.detail = .headline
        }
        func verificationInsets() -> [CameraRef] { stage.insetBoard.items.filter { $0.kind == .verification }.map(\.camera) }
        when({ alerts.requests(trigger: t1).count == 2 }, timeout: 3) { [self] in
            let whileRunning = verificationInsets()
            let panelWhileRunning = !alerts.panelChecks.isEmpty
            answerCheck(t1, Self.personBoxes)
            when({ alerts.board.check(t1)?.decision == "alert" }, timeout: 3) { [self] in
                let opened = verificationInsets()
                let overlay = stage.verificationInsetOverlay(for: a)?.content
                let header = stage.alertInsetViews[a]?.accessibilityLabel() ?? "-"
                print(
                    "verify view popup: while running \(whileRunning.count) insets, confirmed \(opened.count), "
                        + "headline \(overlay?.headline ?? "-"), header \(header)")
                if !whileRunning.isEmpty || panelWhileRunning {
                    problems.append("a check opened an inset or window before it confirmed")
                }
                if opened != [a] { problems.append("the confirmed check's inset is \(opened), not [\(a)]") }
                if overlay?.headline.hasPrefix("Confirmed: person at \(model.name(of: a))") != true {
                    problems.append("the pop-up's headline is \(overlay?.headline ?? "-")")
                }
                if !header.contains("ago") && !header.contains("just now") { problems.append("the inset's heading lacks the elapsed time: \(header)") }
                alerts.dismissAll()
                guard let t2 = smartTrigger(b, kind: "people") else { return checkLog() }
                when({ alerts.requests(trigger: t2).count == 2 }, timeout: 3) { [self] in
                    answerCheck(t2, [])
                    when({ alerts.board.check(t2)?.decision != nil }, timeout: 3) { [self] in
                        let decided = alerts.board.check(t2)?.decision ?? "none"
                        print("verify view popup: an empty picture ended \(decided), insets \(verificationInsets().count)")
                        if decided == "alert" { problems.append("an empty picture confirmed the alert") }
                        if !verificationInsets().isEmpty || !alerts.panelChecks.isEmpty {
                            problems.append("a check that was not confirmed opened an inset or window")
                        }
                        alerts.dismissAll()
                        alerts.update {
                            $0.view.popUp = .always
                            $0.view.detail = .fullLog
                        }
                        stage.unfocus()
                        advance(StageView.focusDuration)
                        next()
                    }
                }
            }
        }
    }
}
