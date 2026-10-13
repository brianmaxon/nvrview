import AppKit
import AVFoundation
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

// Camera motion: the self-test's "ptz" steps (scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// An Option key as AppKit delivers it to the window.
    func pressOption(_ keyCode: UInt16, up: Bool = false) {
        guard let nsWindow = window?.window,
            let e = NSEvent.keyEvent(
                with: up ? .keyUp : .keyDown, location: .zero, modifierFlags: up ? [] : [.option],
                timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: nsWindow.windowNumber, context: nil,
                characters: "", charactersIgnoringModifiers: "", isARepeat: false, keyCode: keyCode)
        else { return }
        NSApp.sendEvent(e)
    }

    /// The pad holds a move and renews it, a key does too, the app going
    /// quiet lets the camera stop by itself, presets recall by menu and key,
    /// the lens stays within its range, and a second lens is addressed as
    /// its own camera. Against the fake core's model of the camera, and on
    /// the real core against the mock's.
    func ptzStep() {
        guard let model = app.model, let window else { return checkLog() }
        let stage = window.stage
        guard let camera = model.allCameras.first(where: { $0.lens == 0 && model.channelInfo($0)?.ptz.panTilt == true }),
            let leaf = stage.frames.leaves.first
        else {
            problems.append("no camera that pans and tilts")
            return checkLog()
        }
        let original = leaf.camera
        stage.assign(camera, at: leaf.path)
        window.state.selected = leaf.path
        window.window?.makeFirstResponder(stage)
        let fake = model.fake
        let ptz = model.ptz!
        func finishPTZ() {
            stage.closePTZPopover()
            ptz.frozenForTest = false
            ptz.stopAll()
            stage.assign(original, at: leaf.path)
            checkLog()
        }
        func sent(_ action: String, _ reason: String? = nil) -> Int {
            ptz.events.filter { $0.action == action && $0.state == .sent && (reason == nil || $0.reason == reason) }.count
        }
        when({ model.leases.tile(for: camera) != nil }, timeout: 5) { [self] in
            ptz.load(camera)
            when({ ptz.info(for: camera) != nil }, timeout: 10) { [self] in
                guard let info = ptz.info(for: camera), let tile = model.leases.tile(for: camera) else {
                    problems.append("the camera's capabilities were not read")
                    return finishPTZ()
                }
                // parity: ptz-pad-hold
                stage.performHeaderItem(.panTilt, at: leaf.path, from: nil)
                guard let pad = stage.ptzPopoverController?.pad[.left] else {
                    problems.append("the pan and tilt pad did not open")
                    return finishPTZ()
                }
                let c0 = ptz.calls.count
                let moves0 = sent("move")
                pad.pressDown()
                for _ in 0..<12 { advance(0.1) }
                let held = Array(ptz.calls[c0...])
                let gaps = zip(held, held.dropFirst()).map { $1.at - $0.at }
                let renewed = held.filter { $0.text.hasPrefix("move left") }.count
                let movingNow = fake.map { $0.ptzMoving(channel: Int(tile.channel)) } ?? true
                pad.pressUp()
                print("ptz pad: \(renewed) move calls over 1.2 s, longest gap \(gaps.max() ?? 0) s, held until release \(movingNow)")
                if renewed < 4 || (gaps.max() ?? 1) > 0.5 { problems.append("a held pad did not renew the move at least every 500 ms") }
                if !movingNow { problems.append("the camera stopped while the pad was held") }
                if ptz.calls.last?.text != "stop" || ptz.held != nil { problems.append("releasing the pad did not stop the camera") }
                if let fake {
                    if fake.ptzMoving(channel: Int(tile.channel)) { problems.append("the camera still moves after the pad's release") }
                    if fake.ptzCalls.filter({ $0.text == "move left 20" }).count != 1 { problems.append("renewals were counted as moves") }
                }
                stage.closePTZPopover()
                when({ sent("move") > moves0 && sent("stop", "released") >= 1 }, timeout: 5) { [self] in
                    if sent("move") <= moves0 || sent("stop", "released") < 1 {
                        problems.append("the core did not report the pad's move and stop")
                    }
                    // The Option arrow holds the move; its release stops it.
                    pressOption(ViewerKeys.rightArrow)
                    advance(0.3)
                    let keyHeld = ptz.held == camera
                    pressOption(ViewerKeys.rightArrow, up: true)
                    if !keyHeld || ptz.held != nil || ptz.calls.last?.text != "stop" {
                        problems.append("Option+arrow did not move the camera while held and stop it on release")
                    }
                    // parity: ptz-deadman
                    pressOption(ViewerKeys.upArrow)
                    advance(0.3)
                    ptz.frozenForTest = true
                    let deadman0 = sent("stop", "deadman")
                    app.testClock?.advance(by: 1.2)
                    let waitForCore = fake == nil
                    when({ fake.map { !$0.ptzMoving(channel: Int(tile.channel)) } ?? (sent("stop", "deadman") > deadman0) },
                        timeout: waitForCore ? 6 : 0.1
                    ) { [self] in
                        let stopped = fake.map { !$0.ptzMoving(channel: Int(tile.channel)) } ?? (sent("stop", "deadman") > deadman0)
                        print("ptz dead-man: camera stopped by itself \(stopped)")
                        if !stopped { problems.append("the camera was not stopped by itself after the app went quiet") }
                        ptz.frozenForTest = false
                        pressOption(ViewerKeys.upArrow, up: true)
                        ptzPresets(finishPTZ)
                    }
                }
                _ = info
            }
        }

        func ptzPresets(_ next: @escaping () -> Void) {
            guard let info = ptz.info(for: camera), let tile = model.leases.tile(for: camera) else { return next() }
            // parity: ptz-preset-recall
            let menu = presetsMenu(info: info, camera: camera, ptz: ptz)
            let base = sent("preset")
            if let item = menu.items.first, let action = item.action {
                NSApp.sendAction(action, to: item.target, from: item)
            }
            if info.presets.count > 1 { pressOption(19); pressOption(19, up: true) }
            let wanted = min(info.presets.count, 2)
            when({ sent("preset") >= base + wanted }, timeout: 5) { [self] in
                let got = sent("preset") - base
                print("ptz presets: \(info.presets.count) listed, \(got) recalled by menu and Option+2")
                if got != wanted { problems.append("preset recalls by menu and key did not reach the camera (\(got) of \(wanted))") }
                if let fake {
                    let texts = fake.ptzCalls.map(\.text).filter { $0.hasPrefix("preset") }
                    if texts != ["preset 0", "preset 1"].prefix(wanted).map({ $0 }) { problems.append("the camera recalled \(texts)") }
                }
                // parity: ptz-lens-clamp
                guard let zoom = info.zoom else { return ptzLens1(next) }
                for _ in 0..<(Int(zoom.max - zoom.min) / max(1, (zoom.max - zoom.min) / 20) + 6) {
                    pressOption(24)
                    pressOption(24, up: true)
                }
                try? model.bridge.ptzLens(tile, .zoom, position: zoom.max + 5000)
                when({ sent("lens") >= 1 }, timeout: 5) { [self] in
                    let position = ptz.lensPosition(camera, .zoom)
                    print("ptz lens: zoom held at \(position ?? -1) of \(zoom.min)...\(zoom.max)")
                    if position != zoom.max { problems.append("Option+= went past the lens range (\(position ?? -1))") }
                    let last = ptz.events.last(where: { $0.action == "lens" && $0.state == .sent })
                    if last?.position != zoom.max { problems.append("the core did not clamp a position past the range (\(last?.position ?? -1))") }
                    if let f = fake, f.ptzLensPositions(channel: Int(tile.channel))?.zoom != zoom.max {
                        problems.append("the camera's zoom was not at its limit")
                    }
                    // A drag sends at most five positions a second and the last one at its end.
                    stage.performHeaderItem(.lens, at: leaf.path, from: nil)
                    let calls0 = fake?.ptzCalls.count ?? 0
                    if let row = stage.ptzPopoverController?.lensRows[.zoom] {
                        for i in 0..<5 { row.drag(to: zoom.min + i * 10, ended: false) }
                        row.drag(to: zoom.min + 50, ended: true)
                        if let f = fake, f.ptzCalls.count - calls0 != 2 {
                            problems.append("a lens drag sent \(f.ptzCalls.count - calls0) positions instead of the first and the last")
                        }
                    } else {
                        problems.append("the lens popover has no zoom slider")
                    }
                    stage.closePTZPopover()
                    ptzLens1(next)
                }
            }
        }

        func ptzLens1(_ next: @escaping () -> Void) {
            // parity: ptz-lens1-addressing
            guard let second = model.allCameras.first(where: { $0.lens == 1 && model.channelInfo($0)?.ptz(lens: 1).panTilt == true })
            else {
                print("ptz second lens: no camera whose second lens moves")
                return next()
            }
            let firstMoves = model.channelInfo(second.firstLens)?.ptz(lens: 0).panTilt == true
            model.leases.acquire(second)
            when({ model.leases.tile(for: second) != nil }, timeout: 5) { [self] in
                ptz.load(second)
                when({ ptz.info(for: second) != nil }, timeout: 10) { [self] in
                    let base = sent("move")
                    ptz.press(.up, camera: second)
                    when({ sent("move") > base }, timeout: 5) { [self] in
                        let failed = ptz.events.contains { $0.state == .failed || $0.state == .refused }
                        ptz.release()
                        let lens = fake?.ptzCalls.last(where: { $0.text.hasPrefix("move up") })?.lens
                        print("ptz second lens: addressed lens \(lens.map(String.init) ?? "on the device"), first lens moves \(firstMoves)")
                        if sent("move") <= base || failed { problems.append("the second lens's move was not accepted") }
                        if let fake, lens != 1 { problems.append("the second lens's move was addressed to lens \(lens ?? -1)") }
                        if firstMoves { problems.append("the first lens of the multi-lens camera claims to move") }
                        model.leases.release(second)
                        next()
                    }
                }
            }
        }
    }
}
