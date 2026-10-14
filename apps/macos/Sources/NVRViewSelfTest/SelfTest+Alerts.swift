@testable import NVRViewApp
import AppKit
import AVFoundation
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

// Smart alerts: settings, live checks, the events log, the detector, end to end: the self-test's "alerts" steps (scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// A trigger from the app's side (the fake core checks it like its own).
    func smartTrigger(_ camera: CameraRef, kind: String, source: String = "device") -> UInt64? {
        guard let model = app.model, let fake = model.fake, let c = model.alertCamera(camera) else { return nil }
        let id = try? fake.alertsTriggerPut(json: #"{"source":"\#(source)","camera":\#(c.json),"kind":"\#(kind)","state":"start"}"#)
        fake.completeAlertWork()
        return id
    }

    /// Plays the detector: the boxes go to the live view and to the core for
    /// each of the check's open requests.
    func answerCheck(_ trigger: UInt64, _ boxes: [AlertDetection]) {
        guard let model = app.model, let fake = model.fake else { return }
        for r in model.alertsModel.requests(trigger: trigger) {
            model.alertsModel.recordDetections(request: r.request, detections: boxes)
            try? fake.alertDetectionsPut(request: r.request, json: AlertDetection.json(boxes))
        }
        fake.completeAlertWork()
    }

    /// A camera check from trigger to decision: waits for its two pictures,
    /// answers them, and waits for the decision.
    func scriptedCheck(_ camera: CameraRef, then next: @escaping @MainActor (UInt64?) -> Void) {
        guard let alerts = app.model?.alertsModel, let t = smartTrigger(camera, kind: "people") else { return next(nil) }
        when({ alerts.requests(trigger: t).count == 2 }, timeout: 3) { [self] in
            answerCheck(t, Self.personBoxes)
            when({ alerts.board.check(t)?.decision != nil }, timeout: 3) { next(t) }
        }
    }

    func smartAlertsSettingsStep() {
        guard let model = app.model, let fake = model.fake, let key = model.runtime("fake-1")?.key else { return checkLog() }
        let alerts = model.alertsModel!
        // parity: alerts-settings-roundtrip
        // Settings > Smart Alerts: an edit is saved and sent to the core, the
        // cameras named by this run's device IDs; the sensor's password goes
        // to the credential store and into the document for the core only,
        // never into the saved settings.
        var sensor = MQTTSensor(key: "sensor-1", name: "Sensor 1")
        sensor.host = "broker.local"
        sensor.topic = "example/sensors/motion"
        sensor.cameras = [
            SensorCamera(
                camera: CameraRef(device: key, channel: 1),
                roi: [ROIPoint(x: 0, y: 0.5), ROIPoint(x: 1, y: 0.5), ROIPoint(x: 1, y: 1)])
        ]
        alerts.update { s in
            s.enabled = true
            s.mode = .away
            s.location = AlertLocation(lat: 12.34, lon: 56.78)
            s.sensors = [sensor]
        }
        alerts.setSecret("example-password", for: .mqttPassword("sensor-1"))
        fake.completeAlertWork()
        when({ alerts.configured != nil }, timeout: 3) { [self] in
            let file = String(decoding: alerts.settings.fileData(), as: UTF8.self)
            let sent = fake.lastAlertsConfiguration ?? ""
            let doc = (try? JSONSerialization.jsonObject(with: Data(sent.utf8))) as? [String: Any] ?? [:]
            let device = model.runtime("fake-1")?.deviceID ?? 0
            let source = (doc["sources"] as? [[String: Any]])?.first
            let camera = (source?["cameras"] as? [[String: Any]])?.first?["camera"] as? [String: Any]
            let secrets = doc["secrets"] as? [String: String]
            print(
                "smart alerts settings: sent \(sent.count) bytes, mode \(doc["mode"] ?? "-"), "
                    + "location \(doc["location"] ?? "-"), sensor camera \(camera ?? [:]), file \(file.count) bytes")
            if SmartAlertSettings.fromFile(alerts.settings.fileData()) != alerts.settings {
                problems.append("the smart-alert settings did not survive their file")
            }
            if file.contains("example-password") { problems.append("the settings file holds the sensor's password") }
            if doc["enabled"] as? Bool != true || doc["mode"] as? String != "away"
                || (doc["location"] as? [String: Double]) != ["lat": 12.3, "lon": 56.8]
            {
                problems.append("the core did not get the edited settings: \(sent.prefix(200))")
            }
            if (camera?["device"] as? Int).map(UInt64.init) != device || camera?["channel"] as? Int != 1 {
                problems.append("the sensor's camera was not named by this run's device ID")
            }
            if secrets?["sensor-1"] != "example-password" || !alerts.hasSecret(.mqttPassword("sensor-1")) {
                problems.append("the sensor's password did not go from the credential store to the core")
            }
            nightLocationStep()
            verifyTileStep()
        }
    }

    /// Settings > Smart Alerts > Night: the location typed the way people
    /// write it, refusals that keep what was there, and today's predicted
    /// sunrise, sunset, and night window.
    func nightLocationStep() {
        guard let alerts = app.model?.alertsModel else { return }
        let editor = alerts.locationEditor
        // parity: alerts-night-location
        // Switching the location on stores nothing; text that does not read
        // gives a message and changes nothing; "12.3° N" and "45.6 W" are
        // stored as 12.3 and -45.6 and shown that way; a pasted "lat, lon"
        // fills both; the sun times follow the location and the margins.
        let before = alerts.settings.location
        editor.setOn(false)
        editor.setOn(true)
        if alerts.settings.location != nil || !editor.isOn {
            problems.append("switching the location on stored \(String(describing: alerts.settings.location))")
        }
        editor.latText = "abc"
        editor.lonText = "45.6 W"
        editor.set()
        if editor.message == nil || alerts.settings.location != nil || editor.latText != "abc" {
            problems.append("unreadable text was not refused and kept: \(editor.message ?? "no message")")
        }
        editor.latText = "12.3° N"
        editor.lonText = "45.6 W"
        editor.set()
        let want = AlertLocation(lat: 12.3, lon: -45.6)
        if alerts.settings.location != want || editor.message != nil || editor.latText != "12.3" || editor.lonText != "-45.6" {
            problems.append(
                "\"12.3° N\", \"45.6 W\" gave \(String(describing: alerts.settings.location)), fields \(editor.latText) \(editor.lonText), \(editor.message ?? "no message")"
            )
        }
        let utc = TimeZone(identifier: "UTC")!
        let noon = Date(timeIntervalSince1970: 1_718_971_200)
        var lines: [String] = []
        if let times = alerts.sunTimes(now: noon, zone: utc), let rise = times.sunrise, let set = times.sunset,
            let start = times.nightStart, let end = times.nightEnd
        {
            lines = times.lines(zone: utc, locale: Locale(identifier: "en_US"))
            if !(rise < set && set < start && start < end) || lines.count != 2 {
                problems.append("the sun times are out of order: \(lines)")
            }
            alerts.update { $0.night.afterSunsetMin = 60 }
            if let later = alerts.sunTimes(now: noon, zone: utc)?.nightStart, later.timeIntervalSince(start) != 1800 {
                problems.append("the night start did not follow the margin: \(later.timeIntervalSince(start)) s")
            }
            alerts.update { $0.night.afterSunsetMin = 30 }
        } else {
            problems.append("no sun times for a location")
        }
        editor.latText = "99"
        editor.lonText = "x"
        editor.set()
        if editor.message == nil || alerts.settings.location != want || editor.latText != "99" || editor.lonText != "x" {
            problems.append("bad input changed the stored location or the typed text: \(editor.message ?? "no message")")
        }
        editor.latText = "1.5, \u{2212}2.5"
        editor.lonText = ""
        editor.set()
        if alerts.settings.location != AlertLocation(lat: 1.5, lon: -2.5) || editor.lonText != "-2.5" {
            problems.append("a pasted pair did not fill both fields: \(String(describing: alerts.settings.location))")
        }
        print("night location: stored \(String(describing: alerts.settings.location)), sun times \(lines)")
        alerts.update { $0.location = before }
        editor.setOn(before != nil)
    }

    func verifyTileStep() {
        guard let model = app.model, let window else { return checkLog() }
        let alerts = model.alertsModel!
        let stage = window.stage
        guard let camera = stage.frames.leaves.first?.camera else { return checkLog() }
        // parity: verify-view-tile
        // A scripted check of a camera on screen: its band shows in the
        // tile with the detector's boxes (a hit, and one below the floor,
        // labeled) and its steps through the decision; no inset opens.
        scriptedCheck(camera) { [self] t in
            let check = t.flatMap { alerts.board.check($0) }
            let content = stage.verificationBand(for: camera)?.content
            print(
                "verify view tile: \(check?.decision ?? "no decision"), band \(content != nil), boxes "
                    + "\(content?.boxes.map(\.caption) ?? []), last step \(content?.lines.last ?? "-")")
            if check?.decision != "alert" { problems.append("the scripted check did not alert") }
            if content == nil { problems.append("the check's band did not show in the camera's tile") }
            if content?.boxes.map(\.verdict) != [.hit, .belowFloor] {
                problems.append("the band's boxes are \(content?.boxes.map(\.caption) ?? [])")
            }
            if content?.lines.contains(where: { $0.hasPrefix("Decision: alert") }) != true {
                problems.append("the band's steps lack the decision: \(content?.lines ?? [])")
            }
            if stage.insetBoard.items.contains(where: { $0.kind == .verification }) {
                problems.append("a check of a camera on screen opened an inset")
            }
            // parity: verify-view-place-sticky
            // A check keeps the place it started in: with another tile
            // focused (the camera no longer on screen) its band stays in the
            // tile and no inset opens for it.
            if let other = stage.frames.leaves.dropFirst().first {
                stage.focus(other.path)
                advance(StageView.focusDuration)
                stage.syncVerification()
                let kept = stage.verificationBand(for: camera) != nil
                    && !stage.insetBoard.items.contains(where: { $0.kind == .verification })
                print("verify view place: band stays in its tile after another tile is focused \(kept)")
                if !kept { problems.append("a check jumped from its tile to an inset when the stage changed") }
                stage.unfocus()
                advance(StageView.focusDuration)
            }
            smartTileCamera = camera
            verifyDismissStep(t ?? 0, camera, alertID: check?.alertID)
        }
    }

    func verifyDismissStep(_ t: UInt64, _ camera: CameraRef, alertID: UInt64?) {
        guard let model = app.model, let fake = model.fake, let window, let alertID else { return checkLog() }
        let alerts = model.alertsModel!
        let stage = window.stage
        // parity: verify-view-dismiss
        // On the viewer clock: an alert's check stays while its episode
        // lasts and at least 60 s after it settles; then the close button
        // and Dismiss All.
        advance(300)
        alerts.tick()
        let whileActive = stage.verificationBand(for: camera) != nil
        fake.settleAlert(alertID)
        fake.completeAlertWork()
        when({ alerts.board.check(t)?.settledAt != nil }, timeout: 3) { [self] in
            advance(59)
            alerts.tick()
            let at59 = stage.verificationBand(for: camera) != nil
            advance(2)
            alerts.tick()
            let at61 = alerts.board.check(t) == nil && stage.verificationBand(for: camera) == nil
            print("verify view dismiss: shown while active \(whileActive), at 59 s \(at59), gone at 61 s \(at61)")
            if !whileActive || !at59 || !at61 { problems.append("a check's time on screen is wrong") }
            guard let other = stage.frames.leaves.dropFirst().first?.camera,
                let a = smartTrigger(camera, kind: "smoke_alarm", source: "local_audio"),
                let b = smartTrigger(other, kind: "smoke_alarm", source: "local_audio")
            else { return checkLog() }
            when({ alerts.board.check(a)?.decision != nil && alerts.board.check(b)?.decision != nil }, timeout: 3) { [self] in
                stage.verificationBand(for: camera)?.clickCloseForTest()
                let closed = alerts.board.check(a) == nil && alerts.board.check(b) != nil
                alerts.dismissAll()
                let all = alerts.board.shown.isEmpty && stage.verificationBand(for: other) == nil
                print("verify view dismiss: close button \(closed), dismiss all \(all)")
                if !closed { problems.append("the band's close button did not close its check alone") }
                if !all { problems.append("Dismiss All left checks on screen") }
                verifyInsetStep()
            }
        }
    }

    func verifyInsetStep() {
        guard let model = app.model, let window else { return checkLog() }
        let alerts = model.alertsModel!
        let stage = window.stage
        let leaves = stage.frames.leaves
        guard leaves.count >= 3, let a = leaves[1].camera, let b = leaves[2].camera else { return checkLog() }
        // parity: verify-view-inset
        // With another tile focused, checks of cameras the window does not
        // show open verification insets (the alert insets' column), newest
        // on top, without taking the keyboard; Esc closes the top one and
        // its check.
        stage.focus(leaves[0].path)
        advance(StageView.focusDuration)
        let key = NSApp.keyWindow
        let responder = window.window?.firstResponder
        scriptedCheck(a) { [self] t1 in
            scriptedCheck(b) { [self] t2 in
                let insets = stage.insetBoard.shown.filter { $0.kind == .verification }.map(\.camera)
                let overlay = stage.verificationInsetOverlay(for: b)?.content
                let focusKept = NSApp.keyWindow === key && window.window?.firstResponder === responder
                print(
                    "verify view inset: insets \(insets), overlay boxes \(overlay?.boxes.count ?? 0), "
                        + "focus kept \(focusKept)")
                if insets != [b, a] { problems.append("the checks' insets are \(insets), not the newest on top") }
                if overlay?.boxes.count != 2 { problems.append("the inset did not show the check's boxes") }
                if !focusKept { problems.append("a check's inset took the keyboard") }
                _ = stage.handleInsetKey(.dismiss)
                if let t2, alerts.board.check(t2) != nil || stage.insetBoard.item(for: b) != nil {
                    problems.append("Esc did not close the top check and its inset")
                }
                // The window setting: off-screen checks go to the small
                // window, which never becomes key.
                alerts.update { $0.view.placement = .tileThenWindow }
                let panelShown = app.verificationPanel?.window?.isVisible == true
                let panelKey = app.verificationPanel?.window?.isKeyWindow == true
                let insetGone = stage.insetBoard.item(for: a) == nil
                print("verify view window: shown \(panelShown), key \(panelKey), inset gone \(insetGone)")
                if !panelShown || panelKey || !insetGone {
                    problems.append("the window setting did not move the check to a window that never takes focus")
                }
                alerts.dismissAll()
                alerts.update { $0.view.placement = .tileThenInset }
                if app.verificationPanel?.window?.isVisible == true { problems.append("the empty checks window stayed") }
                _ = t1
                stage.unfocus()
                advance(StageView.focusDuration)
                verifyStickStep()
            }
        }
    }

    /// A mouse event through the window's real routing, at a point in the
    /// stage's overlay (top-left origin).
    func stageMouse(_ type: NSEvent.EventType, _ p: CGPoint, in controller: ViewerWindowController) {
        guard let nsWindow = controller.window else { return }
        let at = controller.stage.tileOverlay.convert(p, to: nil)
        guard
            let e = NSEvent.mouseEvent(
                with: type, location: at, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: nsWindow.windowNumber, context: nil, eventNumber: 0, clickCount: 1,
                pressure: type == .leftMouseUp ? 0 : 1)
        else { return }
        nsWindow.sendEvent(e)
    }

    /// The view the window's content view hands a point in the stage's
    /// overlay (what a click there would reach).
    func stageHit(_ p: CGPoint, in controller: ViewerWindowController) -> NSView? {
        guard let content = controller.window?.contentView, let frameView = content.superview else { return nil }
        return content.hitTest(frameView.convert(controller.stage.tileOverlay.convert(p, to: nil), from: nil))
    }

    func verifyStickStep() {
        guard let model = app.model, let window else { return checkLog() }
        let alerts = model.alertsModel!
        let stage = window.stage
        let leaves = stage.frames.leaves
        guard leaves.count >= 3, let a = leaves[1].camera, let b = leaves[2].camera else { return checkLog() }
        // parity: verify-view-stick
        // Clicking a verification inset puts its camera on the stage, and it
        // stays there through the checks' refreshes (the next step of a
        // check, the window's slow timer) until the user swaps back.
        // parity: verify-view-order
        // With two insets up, each inset's overlay is directly above its own
        // inset, and refreshes do not move the views.
        stage.focus(leaves[0].path)
        advance(StageView.focusDuration)
        scriptedCheck(a) { [self] _ in
            scriptedCheck(b) { [self] _ in
                @MainActor func tail() -> [ObjectIdentifier] {
                    stage.insetBoard.shown.reversed().flatMap { item -> [NSView] in
                        [stage.alertInsetViews[item.camera], stage.verificationInsetOverlay(for: item.camera)].compactMap { $0 }
                    }.map(ObjectIdentifier.init)
                }
                let subviews = { @MainActor in stage.insetLayer.subviews.map(ObjectIdentifier.init) }
                let expected = tail()
                let overlays = [a, b].compactMap { stage.verificationInsetOverlay(for: $0) }
                let moves = overlays.map(\.reparentCount)
                var orderOK = overlays.count == 2 && expected.count == 4
                for _ in 0..<5 {
                    advance(2)
                    stage.syncVerification()
                    stage.refreshChrome()
                    stage.relayoutChrome()
                    stage.layoutInsetViews()
                    let now = subviews()
                    if let first = now.firstIndex(of: expected[0]), Array(now[first...].prefix(4)) != expected { orderOK = false }
                }
                let stable = overlays.map(\.reparentCount) == moves
                print("verify view order: stack \(orderOK), views left in place \(stable)")
                if !orderOK { problems.append("the insets and their overlays are not stacked each overlay above its inset") }
                if !stable { problems.append("a refresh re-added the verification overlays") }

                // Click the top inset (real events), then refresh.
                guard let top = stage.insetBoard.shown.first?.camera, let rect = stage.insetVideoRects[top] else {
                    problems.append("no verification inset to click")
                    return verifyBandStep()
                }
                mouse(.leftMouseDown, CGPoint(x: rect.midX, y: rect.midY), in: window)
                mouse(.leftMouseUp, CGPoint(x: rect.midX, y: rect.midY), in: window)
                let swapped = stage.insetBoard.swap?.shown == top
                var kept = true
                for _ in 0..<5 {
                    advance(2)
                    stage.syncVerification()
                    stage.refreshChrome()
                    if stage.insetBoard.swap?.shown != top { kept = false }
                }
                print("verify view stick: swapped \(swapped), still enlarged after refreshes \(kept)")
                if !swapped { problems.append("clicking a verification inset did not enlarge it") }
                if !kept { problems.append("an enlarged verification inset went back on a refresh") }
                // A real check finishes within a second of the click (the
                // detector answers); the enlarged view stays until the user
                // swaps back or the inset's own closing time.
                alerts.dismissAll()
                var keptAfterEnd = true
                for _ in 0..<3 {
                    advance(2)
                    stage.syncVerification()
                    stage.refreshChrome()
                    if stage.insetBoard.swap?.shown != top { keptAfterEnd = false }
                }
                print("verify view stick: still enlarged after its check ended \(keptAfterEnd)")
                if !keptAfterEnd { problems.append("an enlarged verification inset went back when its check ended") }
                // The user's way back.
                _ = stage.handleInsetKey(.swap)
                if stage.insetBoard.swap != nil { problems.append("Tab did not swap the enlarged inset back") }
                alerts.dismissAll()
                stage.unfocus()
                advance(StageView.focusDuration)
                verifyBandStep()
            }
        }
    }

    func verifyBandStep() {
        guard let model = app.model, let window else { return checkLog() }
        let alerts = model.alertsModel!
        let stage = window.stage
        // A tile whose drag pans once zoomed (not a fisheye one, which aims).
        let pannable = stage.frames.leaves.first { leaf in
            stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 3, eased: false)
            let kind = stage.dragKind(at: CGPoint(x: leaf.rect.minX + 40, y: leaf.rect.maxY - 8))
            stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 1.0 / 3, eased: false)
            return kind == "pan"
        }
        guard let leaf = pannable, let camera = leaf.camera else { return checkLog() }
        // parity: verify-view-passthrough
        // A band over a tile lets the mouse through except on its own
        // buttons: through the window's real event routing, a drag that
        // starts on the band pans the zoomed tile, and the close button
        // is still the band's.
        scriptedCheck(camera) { [self] _ in
            stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 3, eased: false)
            guard let band = stage.verificationBand(for: camera) else {
                problems.append("no band to test the pass-through on")
                return eventsLogStep()
            }
            let onBand = CGPoint(x: leaf.rect.minX + 40, y: leaf.rect.maxY - 8)
            let closeFrame = band.closeButtonFrameForTest
            let onClose = CGPoint(x: leaf.rect.minX + closeFrame.midX, y: leaf.rect.minY + closeFrame.midY)
            let hit = stageHit(onBand, in: window)
            let throughBand = hit.map { !($0 === band || $0.isDescendant(of: band)) } ?? false
            let hitClose = stageHit(onClose, in: window).map { $0.isDescendant(of: band) } ?? false
            let before = stage.zoom(atTile: leaf.path)
            stageMouse(.leftMouseDown, onBand, in: window)
            for i in 1...4 {
                stageMouse(.leftMouseDragged, CGPoint(x: onBand.x + CGFloat(i) * 12, y: onBand.y - CGFloat(i) * 6), in: window)
            }
            stageMouse(.leftMouseUp, CGPoint(x: onBand.x + 48, y: onBand.y - 24), in: window)
            let panned = stage.zoom(atTile: leaf.path) != before
            print("verify view passthrough: mouse reaches the tile \(throughBand), close button kept \(hitClose), drag pans \(panned)")
            if !throughBand { problems.append("the band took a click that is not on its buttons: \(String(describing: hit))") }
            if !hitClose { problems.append("the band's close button is not reachable") }
            if !panned { problems.append("a drag starting on a band did not pan the tile") }
            alerts.dismissAll()
            stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 1.0 / 3, eased: false)
            let gone = stage.verificationBand(for: camera) == nil
            if !gone { problems.append("a band outlived its dismissed check") }
            eventsLogStep()
        }
    }

    func eventsLogStep() {
        guard let model = app.model, let window, let events = app.openSmartAlertEvents()?.events else { return checkLog() }
        let alerts = model.alertsModel!
        _ = window
        guard let first = smartTileCamera, let firstAlert = model.alertCamera(first) else { return checkLog() }
        // parity: events-log-filter
        // The Events window lists every decision; the filters narrow it and
        // an entry opens its whole trace.
        events.range = .all
        let all = events.entries.count
        events.filter.sources = [.sound]
        let sound = events.entries.map(\.className)
        events.filter.sources = []
        events.filter.camera = firstAlert
        let onFirst = events.entries.allSatisfy { $0.camera == firstAlert } && !events.entries.isEmpty
        if !onFirst { print("events on \(firstAlert): \(events.entries.map { "\($0.camera.map { "\($0.device)/\($0.channel)" } ?? "-") \($0.decision)" })") }
        events.filter.decisions = ["alert"]
        events.filter.text = "person"
        let person = events.entries.first
        events.select(person?.id)
        let lines = events.selected.map { e in e.allSteps.map { TraceText.line($0, cameraName: events.cameraName) } } ?? []
        print(
            "events log: \(all) entries, sound \(sound), on the first camera \(onFirst), "
                + "trace of \(lines.count) steps ending \(lines.last ?? "-")")
        if all < 5 { problems.append("the event log has \(all) entries") }
        if sound.isEmpty || sound.contains(where: { $0 != "smoke" }) { problems.append("the sound filter showed \(sound)") }
        if !onFirst { problems.append("the camera filter let other cameras through") }
        if !lines.contains(where: { $0.hasPrefix("Gate: passed") }) || !lines.contains(where: { $0.hasPrefix("Delivered") }) {
            problems.append("the opened entry's trace is incomplete: \(lines)")
        }
        // parity: events-log-feedback
        // Feedback on an entry is kept and shown; a mute suppresses the
        // camera's next trigger until it is lifted.
        guard let entry = person else { return finishEvents() }
        events.feedback(entry, useful: false, reason: .nothingThere)
        let down = events.selected.map { ($0.vote, $0.voteReason) }
        events.feedback(entry, useful: true)
        let up = events.selected?.vote
        events.mute(entry, .minutes15)
        let muted = alerts.status.map { SmartAlertLogPage(status: $0).muted.map(\.camera) } ?? []
        let t = smartTrigger(first, kind: "smoke_alarm", source: "local_audio")
        when({ t.flatMap { alerts.board.check($0)?.decision } != nil || events.entries.contains { $0.triggerID == t } }, timeout: 3) {
            [self] in
            events.filter = SmartAlertLogFilter()
            let suppressed = events.entries.first { $0.triggerID == t }
            print(
                "events feedback: down \(String(describing: down)), up \(up ?? "-"), muted \(muted), "
                    + "next trigger \(suppressed?.decision ?? "-") (\(suppressed?.reason ?? "-"))")
            if down?.0 != "down" || down?.1 != "nothing_there" || up != "up" {
                problems.append("feedback was not kept: \(String(describing: down)) then \(up ?? "-")")
            }
            if muted != [firstAlert] { problems.append("the mute did not show in the status: \(muted)") }
            if suppressed?.decision != "suppress" || suppressed?.reason != "muted" {
                problems.append("a muted camera's trigger was not suppressed")
            }
            alerts.mute(firstAlert, seconds: 0)
            guard let kept = events.entries.first(where: { $0.triggerID == person?.triggerID }) ?? person else { return finishEvents() }
            Task { @MainActor [self] in
                await eventsImageStep(events, entry: kept)
                finishEvents()
            }
        }
    }

    func finishEvents() {
        app.eventsWindow?.close()
        app.model?.alertsModel.dismissAll()
        app.model?.alertsModel.update { $0.enabled = false }
        checkLog()
    }

    func checkLogNow() {
        guard runs("alerts") else { return checkLogRest() }
        // parity: alerts-detector-benchmark
        // Started here and awaited before the rest, so a slow model load does
        // not delay the earlier steps.
        awaitDetector(DetectorSelfTest())
    }

    func awaitDetector(_ detector: DetectorSelfTest) {
        guard detector.finished else { return after(0.05) { [self] in awaitDetector(detector) } }
        problems += detector.problems
        alertsEndToEndStep()
    }

    /// Smart alerts end to end on the real core (`AlertsSelfTest`).
    func alertsEndToEndStep() {
        guard let model = app.model else { return checkLogRest() }
        if let why = AlertsSelfTest.unavailable(model) {
            print("alerts end to end: skipped (\(why))")
            return checkLogRest()
        }
        // parity: alerts-e2e-verified
        // parity: alerts-e2e-not-confirmed
        // parity: alerts-ui-real-core
        // parity: alerts-sensor-messages
        // parity: alerts-sensor-diagnostics
        // parity: alerts-simulate-trigger
        // parity: alerts-ha-test-alert
        // parity: alerts-evidence-kept
        // parity: alerts-events-overlay
        // parity: alerts-cooldown-logs-second
        // parity: alerts-night-evidence-expiry
        // parity: alerts-cooldown-always-alert
        Task { @MainActor [self] in
            problems += await AlertsSelfTest(model: model, openEvents: { [weak app] in app?.openSmartAlertEvents() }).run()
            checkLogRest()
        }
    }
}
