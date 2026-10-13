import AppKit
import AVFoundation
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

// Doorbell insets and calls, call windows, insets dragged: the self-test's "calls" steps (scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    func insetStep() {
        guard let model = app.model, let window, let fake = model.fake else { return checkLog() }
        guard runs("calls") else { return fisheyeStep() }
        let stage = window.stage
        guard let device = model.runtime("fake-1"), let id = device.deviceID else {
            problems.append("no fake device for the doorbell")
            return checkLog()
        }
        let doorbell = CameraRef(device: device.key, channel: 3)
        guard let other = window.state.tree.leaves.first(where: { $0.camera != nil && $0.camera != doorbell }),
            let otherCamera = other.camera
        else {
            problems.append("no other camera to focus")
            return checkLog()
        }
        model.stopListening()
        stage.focus(other.path)
        advance(StageView.focusDuration)
        let startsBefore = fake.talkStarts
        // parity: call-press-opens-call
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        when({ stage.insetBoard.item(for: doorbell) != nil && model.calls.call(for: doorbell) != nil }, timeout: 3) { [self] in
            guard stage.insetBoard.item(for: doorbell) != nil, let call = model.calls.call(for: doorbell) else {
                problems.append("a doorbell press did not open its inset and call")
                return checkLog()
            }
            let views = stage.shownInsetViews
            print("doorbell: inset \(views.count), unmute shown \(views.first?.unmuteShown == true), talk \(call.talk.text)")
            if views.count != 1 { problems.append("expected one inset view, saw \(views.count)") }
            if !call.showsUnmute || views.first?.unmuteShown != true {
                problems.append("the doorbell call did not show Unmute while nothing was playing")
            }
            if model.audible.contains(doorbell) { problems.append("the doorbell became audible without listening or the setting") }
            if fake.talkStarts != startsBefore { problems.append("talk-back started on its own") }
            // parity: alert-inset-in-grid
            // The inset is one more tile of the same grid, drawn on top.
            let grid = stage.grid
            grid.prepare?(grid.renderer, grid.drawableSize)
            // In focus the grid draws the focused tile; the inset adds one.
            if grid.renderer.tiles.count != 2 {
                problems.append("the grid draws \(grid.renderer.tiles.count) tiles for the focused tile and one inset")
            }
            guard let tile = model.leases.tile(for: doorbell) else {
                problems.append("the doorbell has no tile")
                return checkLog()
            }
            if grid.renderer.tiles.last.map({ ObjectIdentifier($0.picture as AnyObject) }) != ObjectIdentifier(tile) {
                problems.append("the inset is not drawn last (on top)")
            }
            // parity: call-camera-audio-recognized
            // parity: call-captions
            // Its tapped audio reaches the visitor's transcript (fake engine).
            fake.pumpAudio(tile: tile.id, count: 17)
            when({ model.calls.call(for: doorbell)?.transcript.finals.isEmpty == false }, timeout: 2) { [self] in
                let lines = model.calls.call(for: doorbell)?.transcript.lines ?? []
                print("transcript: \(lines.map { "\($0.source.label): \($0.text)" })")
                if lines.first?.source != .visitor { problems.append("the doorbell's audio did not reach the transcript") }
                talkStep(doorbell, otherCamera, startsBefore)
            }
        }
    }

    func talkStep(_ doorbell: CameraRef, _ otherCamera: CameraRef, _ startsBefore: Int) {
        guard let model = app.model, let fake = model.fake, let tile = model.leases.tile(for: doorbell) else {
            return checkLog()
        }
        // parity: call-talk-permission-prompt
        // Without the permission Talk asks first; Cancel sends nothing, the
        // call stays open, and the camera's option stays off.
        guard let prompter = model.calls.fakePrompter else { return checkLog() }
        prompter.answers = [false]
        model.calls.startTalking(doorbell)
        let name = model.name(of: doorbell)
        print("talk without permission: asked \(prompter.asked.map(\.title)), \(model.calls.call(for: doorbell)?.talk.text ?? "-")")
        if prompter.asked.map(\.title) != ["Allow talk-back for \(name)?"] {
            problems.append("Talk without the permission did not ask to allow talk-back for the camera")
        }
        if model.calls.call(for: doorbell)?.talk.phase != .idle { problems.append("a canceled prompt left talk-back started") }
        if fake.talkStarts != startsBefore { problems.append("talk-back reached the device before the user allowed it") }
        if model.cameraOptions(doorbell).allowTalk { problems.append("Cancel turned talk-back on") }
        if model.calls.call(for: doorbell) == nil { problems.append("Cancel ended the call") }
        model.calls.stopTalking(doorbell)
        // A typed reply asks too; Cancel keeps the text (accepted false) and sends nothing.
        prompter.answers = [false]
        var accepted: Bool?
        model.calls.setDraft(doorbell, "Not yet")
        model.calls.sendReply("Not yet", to: doorbell) { accepted = $0 }
        if accepted != false || prompter.asked.count != 2 { problems.append("a typed reply without the permission did not ask and keep its text") }
        if model.calls.call(for: doorbell)?.draft != "Not yet" { problems.append("a canceled typed reply lost the call's draft") }
        if fake.talkStarts != startsBefore || model.calls.fakeSpeech?.rendered.isEmpty == false {
            problems.append("a canceled typed reply was rendered or sent")
        }
        // parity: call-talk-allowed
        // Allow persists the option and starts the talk without pressing
        // again; the microphone's audio goes out while Talk is held.
        prompter.answers = [true]
        model.calls.startTalking(doorbell)
        if !model.cameraOptions(doorbell).allowTalk { problems.append("Allow did not turn talk-back on for the camera") }
        when({ model.calls.call(for: doorbell)?.talk.isActive == true }, timeout: 2) { [self] in
            model.calls.fakeMicrophone?.speak(count: 16_000)
            let sent = fake.talkPushedMilliseconds(tile: tile.id)
            print(String(format: "talk with permission: %@, %.0f ms pushed", model.calls.call(for: doorbell)?.talk.text ?? "-", sent))
            if model.calls.call(for: doorbell)?.talk.isActive != true { problems.append("talk-back did not become active") }
            if abs(sent - 1000) > 1 { problems.append("the microphone's audio was not pushed (\(sent) ms)") }
            model.calls.stopTalking(doorbell)
            when({ model.calls.call(for: doorbell)?.talk.isTalking == false }, timeout: 2) { [self] in
                // parity: call-typed-reply
                // A typed reply: rendered, pushed once active, drained.
                model.calls.setDraft(doorbell, "Hello, one moment please")
                model.calls.sendReply("Hello, one moment please", to: doorbell)
                if model.calls.call(for: doorbell)?.draft.isEmpty != true { problems.append("a sent reply stayed in the call's draft") }
                // (Letting go of Talk above also drained, so "finished"
                // alone does not show that the reply went out.)
                when({
                    guard model.calls.fakeSpeech?.rendered.isEmpty == false else { return false }
                    if case .stopped(let r)? = model.calls.call(for: doorbell)?.talk.phase { return r == "finished" }
                    return false
                }, timeout: 3) { [self] in
                    let phase = model.calls.call(for: doorbell)?.talk.phase
                    print("typed reply: \(model.calls.fakeSpeech?.rendered ?? []), talk \(String(describing: phase))")
                    if model.calls.fakeSpeech?.rendered != ["Hello, one moment please"] {
                        problems.append("the typed reply was not rendered")
                    }
                    if phase != .stopped(reason: "finished") { problems.append("the typed reply's talk did not drain") }
                    // parity: call-caption-echo
                    // The doorbell's microphone hears its speaker play the reply: the
                    // visitor caption that repeats it is dropped; other speech is kept.
                    guard let engine = model.calls.visitorEngine(doorbell) else { return checkLog() }
                    engine.onPartial?("Hello, one moment")
                    engine.onFinal?("Hello, one moment please.")
                    engine.onFinal?("I have a delivery for the house")
                    when({ model.calls.call(for: doorbell)?.transcript.finals.contains { $0.text.hasPrefix("I have a delivery") } == true }, timeout: 2) { [self] in
                        let call = model.calls.call(for: doorbell)
                        let texts = call?.transcript.finals.map(\.text) ?? []
                        print("caption echo: \(texts), shown \(call?.transcript.lines.map(\.text) ?? [])")
                        if texts.contains(where: { $0.hasPrefix("Hello, one moment") }) { problems.append("the reply came back as a visitor caption (echo not dropped)") }
                        if call?.transcript.partials[.visitor] != nil { problems.append("the echo's volatile caption is still shown") }
                        if call?.echo.dropped != 2 { problems.append("expected two dropped echo captions, saw \(call?.echo.dropped ?? -1)") }
                        coreRefusalStep(doorbell, otherCamera, startsBefore: fake.talkStarts)
                    }
                }
            }
        }
    }

    /// The core still refuses (its permission was revoked behind the app's
    /// back): the same prompt appears; Cancel leaves it refused.
    func coreRefusalStep(_ doorbell: CameraRef, _ otherCamera: CameraRef, startsBefore: Int) {
        guard let model = app.model, let fake = model.fake, let prompter = model.calls.fakePrompter,
            let id = model.runtime(doorbell.device)?.deviceID
        else { return checkLog() }
        // macOS only: the core's own refusal (its permission revoked behind
        // the app's back) shows the same prompt. Windows re-sends the setting
        // before every start, so it cannot be out of sync there.
        try? model.bridge.setTalkPermission(device: id, channel: UInt8(doorbell.channel), allowed: false)
        let asked = prompter.asked.count
        prompter.answers = [false]
        // With "Keep talking": the latch is the call's, and a refusal turns it off.
        model.calls.setLatched(doorbell, true)
        when({ prompter.asked.count == asked + 1 }, timeout: 2) { [self] in
            print("core refusal: asked again \(prompter.asked.count == asked + 1), \(model.calls.call(for: doorbell)?.talk.text ?? "-")")
            if fake.talkStarts != startsBefore { problems.append("a refused talk reached the device") }
            if model.calls.call(for: doorbell)?.talk.isTalking == true { problems.append("Cancel after a core refusal left talk-back running") }
            if model.calls.call(for: doorbell)?.latched == true { problems.append("Keep talking stayed on after a refusal") }
            model.calls.stopTalking(doorbell)
            model.setAllowTalk(doorbell, false)
            swapStep(doorbell, otherCamera)
        }
    }

    func swapStep(_ doorbell: CameraRef, _ otherCamera: CameraRef) {
        guard let model = app.model, let window else { return checkLog() }
        let stage = window.stage
        // parity: call-unmute
        model.calls.unmute(doorbell)
        if !model.audible.contains(doorbell) { problems.append("Unmute did not make the doorbell audible") }
        // parity: alert-inset-swap
        // A click swaps: the doorbell fills the stage, the focused camera
        // goes into the return inset. Clicking a call's inset is call activity.
        let clicked = model.clock.now
        stage.swapInset(doorbell)
        if (model.calls.call(for: doorbell)?.lastActivity ?? 0) < clicked { problems.append("clicking the call's inset did not count as call activity") }
        let swapped = stage.insetBoard.swap?.shown == doorbell
            && stage.insetBoard.items.first.map { $0.isReturn && $0.camera == otherCamera } == true
        print("swap: doorbell on the stage \(swapped), wants \(model.leases.wantedTier(doorbell).name)")
        if !swapped { problems.append("clicking the inset did not swap it with the focused camera") }
        if model.leases.wantedTier(doorbell) != .main { problems.append("the swapped-in doorbell did not ask for the main stream") }
        if !stage.showsCamera(doorbell) || stage.showsCamera(otherCamera) {
            problems.append("after the swap the stage does not show the doorbell")
        }
        press("\t", keyCode: ViewerKeys.tab)
        let back = stage.insetBoard.swap == nil && stage.insetBoard.item(for: doorbell) != nil
        print("tab: swapped back \(back)")
        if !back { problems.append("Tab did not swap back") }
        // parity: call-close-ends
        press("\u{1b}", keyCode: ViewerKeys.escape)
        let gone = stage.insetBoard.isEmpty && model.calls.call(for: doorbell) == nil
        print("esc: inset dismissed and call ended \(gone), audible \(model.audible.count)")
        if !gone {
            problems.append("Esc did not dismiss the inset and end the call: \(stage.insetBoard.items.map { "\($0.camera) \($0.kind)" })")
        }
        if !model.audible.isEmpty { problems.append("ending the call did not restore what played before") }
        if window.state.focused == nil { problems.append("Esc on an inset also left focus") }
        autoCloseStep(doorbell)
    }

    /// A doorbell inset closes by itself a minute after the press ended.
    func autoCloseStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let id = model.runtime("fake-1")?.deviceID else {
            return checkLog()
        }
        let stage = window.stage
        // parity: call-auto-close
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: false, alarm: true)
        when({ stage.insetBoard.item(for: doorbell)?.active == false }, timeout: 3) { [self] in
            advance(InsetBoard.autoCloseDelay - 1)
            let stayed = stage.insetBoard.item(for: doorbell) != nil
            advance(2)
            let closed = stage.insetBoard.item(for: doorbell) == nil
            print("doorbell inset: open after 59 s \(stayed), closed after 61 s \(closed), call ended \(model.calls.call(for: doorbell) == nil)")
            if !stayed || !closed { problems.append("the doorbell inset did not close a minute after the press") }
            if model.calls.call(for: doorbell) != nil { problems.append("the call did not end a minute after the last activity") }
            conversationStep(doorbell)
        }
    }

    /// "Start Conversation" opens the call with no press, sends nothing by
    /// itself, refuses Talk without the permission, and ends through the
    /// same paths.
    func conversationStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake else { return checkLog() }
        let stage = window.stage
        let startsBefore = fake.talkStarts
        // parity: call-conversation-start
        stage.startConversation(doorbell)
        let call = model.calls.call(for: doorbell)
        let opened = stage.insetBoard.item(for: doorbell)?.kind == .conversation
        print("conversation: inset \(opened), presses \(call?.presses ?? -1), audible \(model.audible.contains(doorbell))")
        if !opened || call == nil || call?.presses != 0 || call?.origin != .conversation {
            problems.append("Start Conversation did not open the inset and a call with no press")
        }
        if !model.audible.contains(doorbell) { problems.append("a conversation did not make the camera audible") }
        if fake.talkStarts != startsBefore { problems.append("a conversation sent talk-back by itself") }
        // parity: call-talk-conversation
        // The core is told the call is open on the doorbell's tile (its talk
        // starts skip the per-minute limit), and told again when it ends.
        let markedTile = model.leases.tile(for: doorbell)
        let marked = markedTile.map { fake.inConversation(tile: $0.id) } ?? false
        print("conversation: core told the call is open \(marked)")
        if !marked || model.calls.conversationTile(doorbell) !== markedTile {
            problems.append("opening a conversation did not tell the core (no talk-back limit during a call)")
        }
        model.calls.startTalking(doorbell)
        if model.calls.fakePrompter?.asked.count != 5 || model.calls.call(for: doorbell)?.talk.phase != .idle {
            problems.append("Talk in a conversation did not ask first (and send nothing on Cancel)")
        }
        if fake.talkStarts != startsBefore { problems.append("a canceled Talk in a conversation reached the device") }
        model.calls.stopTalking(doorbell)
        // parity: call-conversation-end
        stage.endConversation(doorbell)
        let ended = model.calls.call(for: doorbell) == nil && stage.insetBoard.item(for: doorbell) == nil
        print("conversation: ended through the close path \(ended), still audible \(model.audible.contains(doorbell))")
        if !ended { problems.append("ending a conversation left its call or inset open") }
        if let markedTile, fake.inConversation(tile: markedTile.id) {
            problems.append("ending a conversation did not tell the core it ended")
        }
        if model.audible.contains(doorbell) { problems.append("ending a conversation left the camera audible") }
        echoStep(doorbell)
    }

    /// Echo cancellation (docs/AUDIO.md "Echo cancellation"): the fake
    /// microphone reports the system's canceller on, then unavailable. Real
    /// echo cannot be heard in a self-test: this checks what the call shows
    /// and logs, and the caption gate's rule in each mode (the microphone is
    /// always sent; only its captions are gated).
    func echoStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let mic = model.calls.fakeMicrophone else {
            return checkLog()
        }
        let stage = window.stage
        // parity: call-echo-cancellation
        stage.startConversation(doorbell)
        guard let tile = model.leases.tile(for: doorbell) else {
            problems.append("the conversation has no tile")
            return transcriptFileStep(doorbell)
        }
        let prepared = mic.prepared
        func youFinals() -> Int { model.calls.call(for: doorbell)?.transcript.finals.filter { $0.source == .you }.count ?? 0 }
        let allowed = model.cameraOptions(doorbell).allowTalk
        model.setAllowTalk(doorbell, true)
        mic.echo = .system
        // The doorbell plays ordinary speech (above the half-duplex gate's
        // threshold, below the safety duck's).
        fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.3, rms: 0.1))
        model.calls.startTalking(doorbell)
        let talking: @MainActor () -> Bool = { model.calls.call(for: doorbell)?.talk.isActive == true && model.calls.echoCancellation(for: doorbell) != nil }
        when(talking, timeout: 2) { [self] in
            let state = model.calls.echoCancellation(for: doorbell)
            let note = model.calls.note(for: doorbell) ?? "-"
            let gateOn = model.calls.echoGateCancelled(doorbell)
            mic.speak(count: 16_000)
            when({ youFinals() > 0 }, timeout: 1) { [self] in
                let fullDuplex = youFinals()
                // Very loud: the short safety duck.
                fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.8, rms: 0.4))
                mic.speak(count: 16_000)
                // The samples were processed inside `speak`; any caption they
                // make is queued behind this hop.
                afterQueued { [self] in
                    let ducked = youFinals() == fullDuplex
                    model.calls.stopTalking(doorbell)
                    print("echo cancellation on: \(String(describing: state)), note \"\(note)\", captions while the doorbell speaks \(fullDuplex > 0), ducked when very loud \(ducked), prepared at call open \(prepared)")
                    if state != .system || gateOn != true { problems.append("the microphone's echo cancellation was not taken up by the call") }
                    if note != "Talking · Echo cancellation on" { problems.append("the call does not show that echo cancellation is on: \(note)") }
                    if fullDuplex == 0 { problems.append("with echo cancellation the microphone was not captioned while the doorbell spoke") }
                    if !ducked { problems.append("with echo cancellation a very loud doorbell did not duck the microphone's captions") }
                    if !prepared { problems.append("opening a call did not prepare echo cancellation") }
                    // The drained talk ends first (its end would stop the next one's microphone).
                    when({ model.calls.call(for: doorbell)?.talk.isTalking == false }, timeout: 3) { [self] in
                        echoFallbackStep(doorbell, tile: tile, allowed: allowed)
                    }
                }
            }
        }
    }

    /// The fallback: no echo canceller; the half-duplex caption gate.
    func echoFallbackStep(_ doorbell: CameraRef, tile: CoreTile, allowed: Bool) {
        guard let model = app.model, let window, let fake = model.fake, let mic = model.calls.fakeMicrophone else {
            return checkLog()
        }
        mic.echo = .gated("self-test: no echo canceller")
        fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.3, rms: 0.1))
        func youFinals() -> Int { model.calls.call(for: doorbell)?.transcript.finals.filter { $0.source == .you }.count ?? 0 }
        let before = youFinals()
        model.calls.startTalking(doorbell)
        let talking: @MainActor () -> Bool = { model.calls.call(for: doorbell)?.talk.isActive == true && model.calls.echoCancellation(for: doorbell) != nil }
        when(talking, timeout: 2) { [self] in
            let pushedBefore = fake.talkPushedMilliseconds(tile: tile.id)
            let note = model.calls.note(for: doorbell) ?? "-"
            let gateOn = model.calls.echoGateCancelled(doorbell)
            mic.speak(count: 16_000)
            afterQueued { [self] in
                let gated = youFinals() == before
                let pushed = fake.talkPushedMilliseconds(tile: tile.id) - pushedBefore
                model.calls.stopTalking(doorbell)
                let log = model.alerts.log.map(\.text)
                let onLogged = log.filter { $0 == "Talk-back microphone: echo cancellation on" }.count
                let offLogged = log.filter { $0.hasPrefix("Talk-back microphone: echo cancellation unavailable (self-test: no echo canceller)") }.count
                print("echo cancellation off: note \"\(note)\", captions gated \(gated), \(Int(pushed)) ms still sent, logged on \(onLogged)x, off \(offLogged)x")
                if gateOn != false { problems.append("without echo cancellation the caption gate did not take over") }
                if note != "Talking · No echo cancellation" { problems.append("the call does not show that echo cancellation is off: \(note)") }
                if !gated { problems.append("without echo cancellation the microphone was captioned while the doorbell spoke") }
                if abs(pushed - 1000) > 1 { problems.append("without echo cancellation the microphone was not sent (\(pushed) ms)") }
                if onLogged != 1 || offLogged != 1 { problems.append("echo cancellation was not logged once per outcome") }
                fake.setAudioLevel(tile: tile.id, nil)
                mic.echo = .system
                model.setAllowTalk(doorbell, allowed)
                window.stage.endConversation(doorbell)
                if mic.prepared { problems.append("the last call ending did not release echo cancellation's preparation") }
                transcriptFileStep(doorbell)
            }
        }
    }

    /// "Save call transcripts" on: a call's transcript is written as a text
    /// file when it ends (in a temporary folder here); off: nothing is.
    func transcriptFileStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake else { return checkLog() }
        let stage = window.stage
        // parity: call-transcript-file
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(
            "nvrview-selftest-transcripts-\(UUID().uuidString)", isDirectory: true)
        let before = model.transcripts.folder
        model.transcripts.folder = folder
        model.saveCallTranscripts = true
        stage.startConversation(doorbell)
        guard let tile = model.leases.tile(for: doorbell) else {
            problems.append("the conversation has no tile")
            return popOutStep(doorbell)
        }
        fake.pumpAudio(tile: tile.id, count: 17)
        when({ model.calls.call(for: doorbell)?.transcript.finals.isEmpty == false }, timeout: 2) { [self] in
            stage.endConversation(doorbell)
            let names = model.transcripts.names()
            let text = names.first.flatMap { try? String(contentsOf: folder.appendingPathComponent($0), encoding: .utf8) } ?? ""
            let camera = CallTranscriptFile.sanitized(model.name(of: doorbell))
            print("transcript file: \(names), \(text.split(separator: "\n").count) lines")
            if names.count != 1 || names.first?.hasSuffix(" \(camera).txt") != true {
                problems.append("ending a call with saving on did not write one transcript file named for the camera: \(names)")
            }
            if !text.hasPrefix("NVRView call transcript\nCamera: \(model.name(of: doorbell))\n")
                || !text.contains("\nCall: conversation started in the app\n") || !text.contains(" Visitor: ")
            {
                problems.append("the transcript file lacks its header or the visitor's line")
            }
            // Off (the default): the next call writes nothing.
            model.saveCallTranscripts = false
            stage.startConversation(doorbell)
            fake.pumpAudio(tile: tile.id, count: 17)
            when({ model.calls.call(for: doorbell)?.transcript.finals.isEmpty == false }, timeout: 2) { [self] in
                stage.endConversation(doorbell)
                if model.transcripts.names().count != 1 { problems.append("a call with saving off wrote a transcript file") }
                try? FileManager.default.removeItem(at: folder)
                model.transcripts.folder = before
                popOutStep(doorbell)
            }
        }
    }

    // MARK: Calls in their own windows (docs/UX.md "A call in its own window")

    /// A press shown in two viewer windows' insets pops out into a call
    /// window: the insets go from both, the window draws exactly the
    /// doorbell's shared tile, and the call (tap, talk, leases) is untouched.
    func popOutStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let calls = app.callWindows,
            let id = model.runtime(doorbell.device)?.deviceID,
            let otherCamera = window.state.tree.cameras.first(where: { $0.firstLens != doorbell })
        else { return smokeStep() }
        // A second viewer window that does not show the doorbell either.
        guard let second = app.openWindow(.scratch(name: "Second", root: .leaf(otherCamera))) else {
            problems.append("could not open a second viewer window")
            return smokeStep()
        }
        let stages = [window.stage, second.stage]
        let baseline = model.leases.leaseCount(doorbell)
        // parity: call-popout-open
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        when({ model.calls.call(for: doorbell) != nil && stages.allSatisfy { !$0.shownInsetViews.isEmpty } }, timeout: 3) { [self] in
            guard let before = model.calls.call(for: doorbell), let tile = model.leases.tile(for: doorbell) else {
                problems.append("a press did not open the call in both viewer windows")
                return finishPopOut(second)
            }
            let insetsBefore = stages.map { $0.shownInsetViews.count }
            let leasesBefore = model.leases.leaseCount(doorbell)
            let starts = fake.talkStarts
            let popButtons = stages.allSatisfy { $0.shownInsetViews.first?.popShown == true }
            model.popOutCall(doorbell)
            guard let c = calls.controller(for: doorbell) else {
                problems.append("Pop Out opened no call window")
                return finishPopOut(second)
            }
            let gone = stages.allSatisfy { stage in
                stage.shownInsetViews.isEmpty && stage.insetBoard.detachedCameras == [doorbell]
                    && !stage.insetBoard.shown.contains { $0.camera == doorbell }
            }
            draw(c)
            let drawn = c.stage.grid.renderer.tiles
            let exact = drawn.count == 1 && drawn.first.map { ObjectIdentifier($0.picture as AnyObject) } == ObjectIdentifier(tile)
            let after = model.calls.call(for: doorbell)
            let same = after?.startedAt == before.startedAt && after?.presses == before.presses
                && after?.transcript == before.transcript && after?.presentation == .window
            let tapped = model.calls.isTapped(doorbell) && fake.pumpAudio(tile: tile.id, count: 1) == 1
            let leases = model.leases.leaseCount(doorbell)
            let wantedLeases = leasesBefore - insetsBefore.reduce(0, +) + 1
            let tier = model.leases.wantedTier(doorbell)
            let tiers = model.leases.tiers(for: doorbell)
            print(
                "pop out: insets \(insetsBefore) -> gone \(gone), window draws the doorbell tile \(exact), same call \(same), "
                    + "tapped \(tapped), leases \(leasesBefore) -> \(leases), main \(c.asksForMainStream) (wants \(tier.name))")
            if !popButtons { problems.append("a call's inset did not offer Pop Out in its header") }
            if !gone { problems.append("Pop Out left the call's inset in a viewer window") }
            if !exact { problems.append("the call window does not draw exactly the doorbell's tile (\(drawn.count) tiles)") }
            if !same { problems.append("Pop Out changed the call") }
            if !tapped { problems.append("Pop Out stopped the doorbell's audio tap") }
            if fake.talkStarts != starts { problems.append("Pop Out started talk-back") }
            if leases != wantedLeases || model.leases.tile(for: doorbell) !== tile {
                problems.append("Pop Out leases: \(leases), expected \(wantedLeases) on the same tile")
            }
            if tier != (c.asksForMainStream ? tiers.focus : tiers.grid) {
                problems.append("the call window's stream demand did not reach the leases (\(tier.name))")
            }
            if c.window?.isKeyWindow == true || NSApp.isActive { problems.append("the call window took the focus in the self-test") }
            callTileStep(doorbell, c) { [self] in
                popOutTalkStep(doorbell, tile: tile, stages: stages, second: second, baseline: baseline)
            }
        }
    }

    /// The call window's picture is a regular tile (docs/CAPTURE.md "The
    /// call window is a regular tile"): a pinch zooms it, the zoom map
    /// shows, the renderer samples the zoomed region, Auto asks for the
    /// main stream, and the header's Reset Zoom eases it back; its menu is
    /// the header model's (Pop In, End Conversation, Record Call) with no
    /// layout items, and focus and split do nothing there.
    func callTileStep(_ doorbell: CameraRef, _ c: CallWindowController, then next: @escaping @MainActor () -> Void) {
        guard let model = app.model else { return next() }
        let stage = c.stage
        draw(c)
        guard stage.fixedTile, stage.frames.leaves.count == 1, let leaf = stage.frames.leaves.first,
            leaf.camera == doorbell, let full = stage.renderedSource(of: leaf.path)
        else {
            problems.append("the call window's picture is not one tile of the doorbell")
            return next()
        }
        // parity: call-window-tile-zoom
        stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 2, eased: false)
        stage.noteInteraction()
        draw(c)
        let zoomed = stage.zoom(atTile: leaf.path)
        let map = stage.zoomMapShown(leaf.path) == true
        let source = stage.renderedSource(of: leaf.path)
        let narrower = source.map { $0.width < full.width * 0.9 || $0.height < full.height * 0.9 } == true
        let wantsMain = c.asksForMainStream && model.leases.wantedTier(doorbell) == model.leases.tiers(for: doorbell).focus
        let resetShown = stage.chromes[leaf.path]?.shownHeaderButtons.contains(.resetZoom) == true
        stage.chromes[leaf.path]?.pressHeader(.resetZoom)
        advance(1)
        draw(c)
        let back = stage.zoom(atTile: leaf.path) == .none && !stage.isEasingZoom
        print(
            String(format: "call window tile: pinch to %.2fx, ", zoomed.scale)
                + "zoom map \(map), source cropped \(narrower), main stream \(wantsMain), "
                + "header Reset Zoom shown \(resetShown) and back to 1x \(back)")
        if abs(zoomed.scale - 2) > 0.01 { problems.append("a pinch did not zoom the call window's tile") }
        if !map { problems.append("the call window's zoomed tile showed no zoom map") }
        if !narrower { problems.append("the call window's renderer did not sample the zoomed region") }
        if !wantsMain { problems.append("the zoomed call window tile did not ask for the main stream") }
        if !resetShown || !back { problems.append("the call window's header did not offer Reset Zoom or did not reset") }
        // parity: call-window-tile-menu
        let menu = stage.contextMenu(for: leaf).items.map(\.title)
        let want = stage.headerTile(doorbell, path: leaf.path).map { TileHeaderLayout.menu(for: $0).map(\.label) } ?? []
        let callItems = ["Pop In Call", "End Conversation", "Record Call", "Save Picture"].allSatisfy(menu.contains)
        let layoutItems = ["Focus", "Split Side by Side", "Remove Tile", "Clear Tile", "Show Camera", "Record Main Stream"]
            .filter(menu.contains)
        stage.toggleFocus()
        stage.split(.row)
        let fixed = stage.state.focused == nil && stage.state.tree.leafCount == 1 && stage.selectedPath == leaf.path
        let header = stage.chromes[leaf.path]?.shownHeaderButtons ?? []
        print(
            "call window tile menu: the model's \(want.count) items \(menu == want), call items \(callItems), "
                + "layout items \(layoutItems), focus and split ignored \(fixed), header \(header.map(\.rawValue))")
        if menu != want || !callItems { problems.append("the call window tile's menu is not the header model's call menu: \(menu)") }
        if !layoutItems.isEmpty { problems.append("the call window tile's menu offers \(layoutItems)") }
        if !fixed { problems.append("focus or split changed the call window's single tile") }
        if !header.contains(.conversation) { problems.append("the doorbell's call window header has no conversation button") }
        callRecordStep(c, doorbell, next)
    }

    /// ⇧⌘R in a call window records the call (docs/CAPTURE.md "Recording
    /// calls"): beside the transcripts under the stem the transcript will
    /// take, the four tracks through the app's AAC encoder, the call bar's
    /// "● Recording" pill, and ⇧⌘R again stops it.
    func callRecordStep(_ c: CallWindowController, _ doorbell: CameraRef, _ next: @escaping @MainActor () -> Void) {
        guard let model = app.model else { return next() }
        // parity: capture-call-record-button
        let answeredBefore = model.capture.encoder?.answered ?? 0
        c.captureRecord(nil)
        let entry = model.capture.callRecording(doorbell)
        let stem = model.capture.reservedCallStem(doorbell)
        whenCapture(20, { $0.call(of: doorbell)?.state == "recording" }) { [self] in
            c.refresh()
            let pill = c.inset.recordingText
            let header = c.stage.headerTile(doorbell, path: c.stage.frames.leaves.first?.path ?? [])
            let label = header.flatMap { TileHeaderLayout.menu(for: $0).first { $0.item == .record }?.label }
            let file = entry.map { URL(fileURLWithPath: $0.file) }
            let besideTranscripts = file?.deletingLastPathComponent().path == model.transcripts.folder.path
            let named = stem.map { file?.lastPathComponent == $0 + ".mp4" } ?? false
            c.captureRecord(nil)
            whenCapture(15, { $0.call(of: doorbell) == nil }) { [self] in
                let ended = model.capture.board.ended.last
                let answered = (model.capture.encoder?.answered ?? 0) - answeredBefore
                print(
                    "call recording: \(file?.lastPathComponent ?? "none"), beside the transcripts \(besideTranscripts), "
                        + "named for the transcript \(named), pill \(pill ?? "none"), Record says \(label ?? "?"), "
                        + "tracks \(ended?.tracks ?? []), ended \(ended?.state ?? "no"), encoder answered \(answered)")
                if entry == nil { problems.append("call recording: ⇧⌘R did not start it (\(c.stage.toastText ?? "no message"))") }
                if !besideTranscripts || !named { problems.append("call recording: not beside the transcripts under its stem") }
                if pill?.hasPrefix("● Recording") != true { problems.append("call recording: the call bar showed no pill") }
                if label != "Stop Recording Call" { problems.append("call recording: Record Call did not become Stop") }
                if ended?.kind != "call" || ended?.tracks != ["video", "Conversation", "Visitor", "You"] {
                    problems.append("call recording: the tracks were \(ended?.tracks ?? [])")
                }
                if answered <= 0 || (model.capture.encoder?.refused ?? 1) != 0 {
                    problems.append("call recording: the AAC encoder answered \(answered) units")
                }
                c.refresh()
                if c.inset.recordingText != nil { problems.append("call recording: the pill stayed after it stopped") }
                advance(ToastView.actionDuration + 0.1)
                next()
            }
        }
    }

    /// Lays out a call window and runs one display refresh's preparation
    /// (a window behind the others gets few refreshes).
    func draw(_ c: CallWindowController) {
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let grid = c.stage.grid
        let scale = c.window?.backingScaleFactor ?? 1
        let b = grid.bounds
        grid.prepare?(grid.renderer, CGSize(width: b.width * scale, height: b.height * scale))
    }

    /// Talk with "Keep talking" and a typed draft survives popping in and
    /// out again: no stop, no new start, the latch and the draft shown.
    func popOutTalkStep(
        _ doorbell: CameraRef, tile: CoreTile, stages: [StageView], second: ViewerWindowController, baseline: Int
    ) {
        guard let model = app.model, let fake = model.fake, let calls = app.callWindows else { return finishPopOut(second) }
        // parity: call-popout-talk
        model.setAllowTalk(doorbell, true)
        model.calls.setLatched(doorbell, true)
        model.calls.setDraft(doorbell, "Be right there")
        when({ model.calls.call(for: doorbell)?.talk.isActive == true }, timeout: 2) { [self] in
            let starts = fake.talkStarts
            model.popInCall(doorbell)
            let insetsShow = stages.allSatisfy { stage in
                stage.shownInsetViews.first.map { $0.callPanel.keepTalkingShown && $0.callPanel.replyText == "Be right there" } == true
            }
            model.popOutCall(doorbell)
            let panel = calls.controller(for: doorbell)?.inset.callPanel
            let windowShows = panel?.keepTalkingShown == true && panel?.replyText == "Be right there"
            // Any stop or start would arrive as an event, queued behind this hop.
            afterQueued { [self] in
                let call = model.calls.call(for: doorbell)
                let kept = call?.talk.isActive == true && fake.isTalking(tile: tile.id) && fake.talkStarts == starts
                print(
                    "pop in and out while talking: talk kept \(kept), latch \(call?.latched == true), draft \(call?.draft ?? "-"), "
                        + "insets show them \(insetsShow), window shows them \(windowShows)")
                if !kept { problems.append("popping in and out stopped or restarted talk-back") }
                if call?.latched != true || call?.draft != "Be right there" { problems.append("popping lost Keep talking or the draft") }
                if !insetsShow { problems.append("the insets did not show Keep talking and the draft after Pop In") }
                if !windowShows { problems.append("the call window did not show Keep talking and the draft") }
                model.calls.setLatched(doorbell, false)
                when({ model.calls.call(for: doorbell)?.talk.isTalking == false }, timeout: 2) { [self] in
                    model.setAllowTalk(doorbell, false)
                    model.calls.setDraft(doorbell, "")
                    newWindowStep(doorbell, stages: stages, second: second, baseline: baseline)
                }
            }
        }
    }

    /// A viewer window opened while the call is popped out records the
    /// call's item, detached, so Pop In shows the inset there too.
    func newWindowStep(_ doorbell: CameraRef, stages: [StageView], second: ViewerWindowController, baseline: Int) {
        guard let model = app.model, let window,
            let otherCamera = window.state.tree.cameras.first(where: { $0.firstLens != doorbell })
        else { return finishPopOut(second) }
        // parity: call-popin-new-window
        guard let third = app.openWindow(.scratch(name: "Third", root: .leaf(otherCamera))) else {
            problems.append("could not open a third viewer window")
            return popInStep(doorbell, stages: stages, second: second, baseline: baseline)
        }
        extraWindows.append(third)
        when({ third.stage.insetBoard.detachedCameras == [doorbell] }, timeout: 1) { [self] in
            let recorded = third.stage.insetBoard.detachedCameras == [doorbell] && third.stage.shownInsetViews.isEmpty
            print("new window while popped out: call recorded detached \(recorded), leases \(model.leases.leaseCount(doorbell))")
            if !recorded { problems.append("a viewer window opened while the call was popped out has no detached item for it") }
            popInStep(doorbell, stages: stages + [third.stage], second: second, baseline: baseline)
        }
    }

    /// Pop In: the window closes, every viewer window shows the inset again
    /// on top with a fresh minute, and the leases follow.
    func popInStep(_ doorbell: CameraRef, stages: [StageView], second: ViewerWindowController, baseline: Int) {
        guard let model = app.model, let calls = app.callWindows else { return finishPopOut(second) }
        // parity: call-popin
        let panel = calls.controller(for: doorbell)?.window
        let leasesBefore = model.leases.leaseCount(doorbell)
        model.popInCall(doorbell)
        let now = model.clock.now
        let back = stages.allSatisfy { stage in
            stage.insetBoard.shown.first?.camera == doorbell && stage.insetBoard.detachedCameras.isEmpty
                && stage.shownInsetViews.first?.callPanel.isHidden == false
                && stage.insetBoard.item(for: doorbell).flatMap(InsetBoard.closesAt).map { $0 >= now + InsetBoard.autoCloseDelay } == true
        }
        let leases = model.leases.leaseCount(doorbell)
        let tiers = model.leases.tiers(for: doorbell)
        let closed = calls.controller(for: doorbell) == nil && panel?.isVisible != true
        print(
            "pop in: window closed \(closed), insets back on top \(back), "
                + "leases \(leasesBefore) -> \(leases), wants \(model.leases.wantedTier(doorbell).name)")
        if !closed { problems.append("Pop In left the call window open") }
        if !back { problems.append("Pop In did not bring the inset back on top, with a fresh minute, in every viewer window") }
        if leases != leasesBefore - 1 + stages.count {
            problems.append("Pop In leases: \(leases), expected \(leasesBefore - 1 + stages.count)")
        }
        if model.leases.wantedTier(doorbell) != tiers.grid { problems.append("the closed call window's stream demand stayed") }
        popOutEndStep(doorbell, stages: stages, second: second, baseline: baseline)
    }

    /// Closing the call window and Esc in it end the call and close the
    /// window; the viewer windows keep nothing. A call in its own window
    /// never ends by inactivity; Pop In restarts the minute.
    func popOutEndStep(_ doorbell: CameraRef, stages: [StageView], second: ViewerWindowController, baseline: Int) {
        guard let model = app.model, let calls = app.callWindows, let window else { return finishPopOut(second) }
        // parity: call-popout-end
        func ended(_ how: String) {
            let clean = model.calls.call(for: doorbell) == nil && calls.controller(for: doorbell) == nil
                && stages.allSatisfy { $0.insetBoard.items.isEmpty } && model.leases.leaseCount(doorbell) == baseline
            print("call window \(how): call ended and window closed \(clean), leases \(model.leases.leaseCount(doorbell))")
            if !clean { problems.append("\(how) did not end the call, close its window and let its insets and leases go") }
        }
        // The close button (and Cmd-W): performClose.
        model.popOutCall(doorbell)
        let first = calls.controller(for: doorbell)?.window
        first?.performClose(nil)
        if first?.isVisible == true { problems.append("the closed call window is still visible") }
        ended("closed")
        // Esc while the reply field is not being edited.
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        if let panel = calls.controller(for: doorbell)?.window,
            let esc = NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: panel.windowNumber, context: nil, characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}",
                isARepeat: false, keyCode: ViewerKeys.escape)
        {
            panel.sendEvent(esc)
        }
        ended("Esc")
        // No expiry while popped out: ten minutes without activity, on the
        // manual clock, leave the call and its window open.
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        advance(10 * DoorbellCall.idleTimeout)
        let stayed = calls.controller(for: doorbell) != nil && model.calls.call(for: doorbell) != nil
        print("call window after 10 min without activity: still open \(stayed)")
        if !stayed { problems.append("a call in its own window ended by itself without activity") }
        // Pop In restores the minute, starting fresh.
        model.popInCall(doorbell)
        advance(DoorbellCall.idleTimeout - 1)
        let fresh = model.calls.call(for: doorbell) != nil
        advance(2)
        print("popped in: open after 59 s \(fresh), ended after 61 s \(model.calls.call(for: doorbell) == nil)")
        if !fresh { problems.append("Pop In did not give the call a fresh minute") }
        ended("expiry after Pop In")
        callWindowFrameStep(doorbell, second: second)
    }

    /// The remembered frame is reused; one whose screen is gone falls back
    /// to the top right of the active viewer window's screen.
    func callWindowFrameStep(_ doorbell: CameraRef, second: ViewerWindowController) {
        guard let model = app.model, let calls = app.callWindows, let window,
            let screen = window.window?.screen ?? NSScreen.screens.first
        else { return finishPopOut(second) }
        // parity: call-window-frame
        let visible = CallWindows.topLeft(screen.visibleFrame)
        let remembered = WindowRect(x: visible.x + 40, y: visible.y + 60, width: 400, height: 360)
        calls.placementMode = .remembered
        calls.rememberedFrame = remembered
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        let reused = calls.controller(for: doorbell)?.window.map { CallWindows.topLeft($0.frame) }
        // The user moves it: that frame is remembered.
        if let panel = calls.controller(for: doorbell)?.window {
            panel.setFrameOrigin(NSPoint(x: panel.frame.minX + 30, y: panel.frame.minY - 20))
        }
        let moved = calls.rememberedFrame
        model.calls.end(doorbell, reason: "closed")
        // A frame on a screen that is gone.
        calls.rememberedFrame = WindowRect(x: -100_000, y: -100_000, width: 400, height: 360)
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        let fallback = calls.controller(for: doorbell)?.window.map { CallWindows.topLeft($0.frame) }
        let m = CallWindowPlacement.margin
        let topRight = WindowRect(x: visible.maxX - m - 400, y: visible.minY + m, width: 400, height: 360)
        model.calls.end(doorbell, reason: "closed")
        func near(_ a: WindowRect?, _ b: WindowRect) -> Bool {
            guard let a else { return false }
            return abs(a.x - b.x) < 0.5 && abs(a.y - b.y) < 0.5 && abs(a.width - b.width) < 0.5 && abs(a.height - b.height) < 0.5
        }
        let movedTo = WindowRect(x: remembered.x + 30, y: remembered.y + 20, width: 400, height: 360)
        print(
            "call window frame: remembered reused \(near(reused, remembered)), move remembered \(near(moved, movedTo)), "
                + "gone screen -> top right of the active screen \(near(fallback, topRight))")
        if !near(reused, remembered) {
            problems.append("the call window did not open at the remembered frame: \(String(describing: reused))")
        }
        if !near(moved, movedTo) { problems.append("moving the call window did not remember its frame: \(String(describing: moved))") }
        if !near(fallback, topRight) {
            problems.append("a remembered frame off every screen did not fall back to the top right: \(String(describing: fallback))")
        }
        callDefaultRingStep(doorbell, second: second)
    }

    /// "Open calls in their own window": a ring opens the call in a call
    /// window that does not take the focus, the viewer windows keep its
    /// item detached, a press that joins keeps the presentation, and the
    /// doorbell's own choice (Inset) wins over the setting.
    func callDefaultRingStep(_ doorbell: CameraRef, second: ViewerWindowController) {
        guard let model = app.model, let window, let fake = model.fake, let calls = app.callWindows,
            let id = model.runtime(doorbell.device)?.deviceID
        else { return finishPopOut(second) }
        let stages = [window.stage, second.stage]
        // parity: call-default-ring
        model.callsOpenInWindow = true
        let saved = calls.defaults.bool(forKey: AppModel.callsOpenInWindowKey)
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        when({ calls.controller(for: doorbell) != nil && stages.allSatisfy { !$0.insetBoard.detachedCameras.isEmpty } }, timeout: 3) {
            [self] in
            let call = model.calls.call(for: doorbell)
            let c = calls.controller(for: doorbell)
            let detached = stages.allSatisfy { $0.insetBoard.detachedCameras == [doorbell] && $0.shownInsetViews.isEmpty }
            let quiet = calls.lastOpenActivated == false && c?.window?.isKeyWindow != true && !NSApp.isActive
            // A press that joins keeps the call where it is, whatever the setting says now.
            model.callsOpenInWindow = false
            fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
            when({ model.calls.call(for: doorbell)?.presses == 2 }, timeout: 3) { [self] in
                let joined = model.calls.call(for: doorbell)
                let kept = joined?.presentation == .window && calls.controller(for: doorbell) === c
                    && stages.allSatisfy { $0.shownInsetViews.isEmpty }
                model.calls.end(doorbell, reason: "closed")
                print(
                    "ring with calls in windows: saved \(saved), window \(call?.presentation == .window && c != nil), "
                        + "opened without focus \(quiet), insets detached \(detached), a joining press kept it \(kept)")
                if !saved { problems.append("Open calls in their own window was not saved") }
                if call?.presentation != .window || c == nil { problems.append("with the setting on, a ring did not open a call window") }
                if !quiet { problems.append("a call window opened by a ring took the focus (or asked to)") }
                if !detached { problems.append("a ring into a call window left an inset or no detached item in a viewer window") }
                if !kept { problems.append("a press joining a call in its window moved it") }
                // The doorbell's own choice wins: Inset with the setting on.
                model.callsOpenInWindow = true
                model.setCallPresentation(doorbell, .inset)
                fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
                when({ stages.allSatisfy { !$0.shownInsetViews.isEmpty } }, timeout: 3) { [self] in
                    let inset = model.calls.call(for: doorbell)?.presentation == .inset && calls.controller(for: doorbell) == nil
                    print("ring with the doorbell set to Inset: insets in both windows \(inset)")
                    if !inset { problems.append("the doorbell's Inset choice did not win over Open calls in their own window") }
                    for stage in stages { stage.dismissInset(doorbell) }
                    model.callsOpenInWindow = false
                    callDefaultConversationStep(doorbell, second: second)
                }
            }
        }
    }

    /// The doorbell's Window choice opens Start Conversation in a call
    /// window (brought forward: the user asked for it) with the setting off;
    /// Default again leaves the camera's options empty.
    func callDefaultConversationStep(_ doorbell: CameraRef, second: ViewerWindowController) {
        guard let model = app.model, let window, let calls = app.callWindows else { return finishPopOut(second) }
        // parity: call-default-conversation
        model.setCallPresentation(doorbell, .window)
        let stored = model.cameraOptions(doorbell)
        window.stage.startConversation(doorbell)
        let call = model.calls.call(for: doorbell)
        let opened = call?.presentation == .window && call?.origin == .conversation && calls.controller(for: doorbell) != nil
        let activates = calls.lastOpenActivated == true
        let detached = window.stage.insetBoard.detachedCameras == [doorbell] && window.stage.shownInsetViews.isEmpty
        model.calls.end(doorbell, reason: "closed")
        model.setCallPresentation(doorbell, nil)
        let cleared = model.cameraOptions(doorbell).callWindow == nil && model.cameraOptions(doorbell).isDefault
        // The saved choice reads back; a file without it reads as Default.
        let encoder = JSONEncoder()
        let roundTrip = (try? encoder.encode(stored)).flatMap { try? JSONDecoder().decode(CameraOptions.self, from: $0) }
        let plain = (try? encoder.encode(CameraOptions(camera: doorbell))).flatMap { String(data: $0, encoding: .utf8) }
        let readsBack = roundTrip?.callPresentation == .window && plain?.contains("callWindow") == false
        print(
            "conversation with the doorbell set to Window: call window \(opened), brought forward \(activates), "
                + "inset detached \(detached), Default clears the option \(cleared), saved choice reads back \(readsBack)")
        if !opened { problems.append("the doorbell's Window choice did not open Start Conversation in a call window") }
        if !activates { problems.append("a call window opened by Start Conversation was not brought forward") }
        if !detached { problems.append("Start Conversation into a call window left an inset in the viewer window") }
        if !cleared { problems.append("choosing Default did not clear the camera's call option") }
        if !readsBack { problems.append("the camera's call choice did not save and read back") }
        finishPopOut(second)
    }

    func finishPopOut(_ second: ViewerWindowController) {
        app.model?.calls.endAll()
        for w in extraWindows { w.close() }
        extraWindows = []
        // The throwaway suite with the frame and pin goes.
        app.callWindows?.defaults.removePersistentDomain(forName: AppDelegate.selfTestDefaults)
        second.close()
        insetDragSnapStep()
    }

    // MARK: Moving the insets (docs/UX.md "Alert insets", "Moving them")

    /// A mouse event as AppKit delivers it to a viewer window (behind the
    /// others, the app inactive), at a point in the stage's inset layer.
    func mouse(_ type: NSEvent.EventType, _ p: CGPoint, in controller: ViewerWindowController) {
        guard let nsWindow = controller.window else { return }
        let at = controller.stage.insetLayer.convert(p, to: nil)
        guard
            let e = NSEvent.mouseEvent(
                with: type, location: at, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: nsWindow.windowNumber, context: nil, eventNumber: 0, clickCount: 1,
                pressure: type == .leftMouseUp ? 0 : 1)
        else { return }
        nsWindow.sendEvent(e)
    }

    /// Presses at `from`, moves to `to` in a few steps, and lets go.
    func drag(from: CGPoint, to: CGPoint, in controller: ViewerWindowController, steps: Int = 5) {
        mouse(.leftMouseDown, from, in: controller)
        for i in 1...steps {
            let t = CGFloat(i) / CGFloat(steps)
            mouse(.leftMouseDragged, CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t), in: controller)
        }
        mouse(.leftMouseUp, to, in: controller)
    }

    /// The anchor the column's frame measures as now (unsnapped).
    static func measuredAnchor(_ column: CGRect, stage: CGRect) -> (x: Double, y: Double) {
        let m = InsetGeometry.margin
        let fx = Double(stage.width) - 2 * m - Double(column.width)
        let fy = Double(stage.height) - 2 * m - Double(column.height)
        return (fx > 0 ? (Double(column.minX) - m) / fx : 0, fy > 0 ? (Double(column.minY) - m) / fy : 0)
    }

    /// A conversation's inset (no press needed) dragged by its header lands
    /// where it was dropped, snapping each axis within 24 pt of an edge; a
    /// small move on the picture is still a click (it swaps); a longer one
    /// drags. The window is saved with the new position.
    func insetDragSnapStep() {
        guard let model = app.model, let window, let device = model.runtime("fake-1") else { return smokeStep() }
        let stage = window.stage
        let doorbell = CameraRef(device: device.key, channel: 3)
        // parity: inset-drag-snap
        stage.startConversation(doorbell)
        guard let view = stage.shownInsetViews.first, let column = stage.insetColumn else {
            problems.append("no inset to drag")
            return smokeStep()
        }
        let b = stage.insetLayer.bounds
        let atDefault = stage.insetAnchor == nil
            && Self.near(Self.measuredAnchor(column, stage: b), InsetAnchor(corner: model.insetCorner))
        // A small move on the picture: a click, which swaps.
        let picture = view.videoRect.offsetBy(dx: view.frame.minX, dy: view.frame.minY)
        let p = CGPoint(x: picture.midX, y: picture.midY)
        drag(from: p, to: CGPoint(x: p.x + 3, y: p.y + 2), in: window, steps: 2)
        let clickSwapped = stage.insetBoard.swap?.shown == doorbell && stage.insetAnchor == nil
        press("\t", keyCode: ViewerKeys.tab)
        guard let header = stage.shownInsetViews.first?.frame, let start = stage.insetColumn else {
            problems.append("the inset did not come back after swapping back")
            return smokeStep()
        }
        // The header, to 10 pt from the right edge (snaps) and 40% down (does not).
        let m = InsetGeometry.margin
        let freeX = Double(b.width) - 2 * m - Double(start.width)
        let freeY = Double(b.height) - 2 * m - Double(start.height)
        let target = CGPoint(x: m + freeX - 10, y: m + 0.4 * freeY)
        let grab = CGPoint(x: header.minX + 40, y: header.minY + 8)
        let saves = savesCount
        drag(from: grab, to: CGPoint(x: grab.x + target.x - start.minX, y: grab.y + target.y - start.minY), in: window)
        let anchor = stage.insetAnchor
        let snapped = anchor.map { $0.x == 1 && abs($0.y - 0.4) < 0.01 } == true
        let placed = stage.insetColumn.map { abs(Double($0.maxX) - (Double(b.width) - m)) < 0.5 } == true
        let saved = window.savedWindow?.insetAnchor == anchor && anchor != nil
        let noSwap = stage.insetBoard.swap == nil
        // The picture past the click's slop drags too (40 pt left: off the snap).
        guard let moved = stage.shownInsetViews.first else { return smokeStep() }
        let pic = moved.videoRect.offsetBy(dx: moved.frame.minX, dy: moved.frame.minY)
        drag(from: CGPoint(x: pic.midX, y: pic.midY), to: CGPoint(x: pic.midX - 40, y: pic.midY), in: window)
        let pictureDrag = stage.insetAnchor.map { abs($0.x - (freeX - 40) / freeX) < 0.01 && abs($0.y - 0.4) < 0.01 } == true
            && stage.insetBoard.swap == nil
        print(
            "inset drag: default corner \(atDefault) at \(Int(column.minX)),\(Int(column.minY)), small move on the picture swaps "
                + "\(clickSwapped), header drag snaps x \(snapped) (anchor \(anchor.map { "\($0.x), \($0.y)" } ?? "-"), right edge "
                + "\(placed)), no swap \(noSwap), saved with the window \(saved), picture drag past the slop \(pictureDrag)")
        if window.window?.isKeyWindow == true || NSApp.isActive { problems.append("the self-test's mouse events took the focus") }
        if !atDefault { problems.append("the inset column did not start in the default corner") }
        if !clickSwapped { problems.append("a click with a small move on the inset's picture did not swap") }
        if !snapped || !placed { problems.append("dragging the inset's header did not snap to the right edge and keep 40% down") }
        if !noSwap { problems.append("dragging an inset swapped it") }
        if !saved || savesCount == saves { problems.append("the moved column was not saved with the window") }
        if !pictureDrag { problems.append("dragging the inset's picture past 6 pt did not move the column") }
        insetDragResizeStep(doorbell)
    }

    static func near(_ a: (x: Double, y: Double), _ b: InsetAnchor) -> Bool {
        abs(a.x - b.x) < 0.01 && abs(a.y - b.y) < 0.01
    }

    /// Resizing the window keeps the column at the same fraction of the
    /// free space (full screen is a resize too; the self-test never enters
    /// it).
    func insetDragResizeStep(_ doorbell: CameraRef) {
        guard let window, let nsWindow = window.window, let anchor = window.stage.insetAnchor else { return insetDragRestoreStep(doorbell) }
        let stage = window.stage
        // parity: inset-drag-resize
        let frame = nsWindow.frame
        var results: [Bool] = []
        for grow in [CGSize(width: 220, height: 120), CGSize(width: -120, height: -60)] {
            nsWindow.setFrame(
                NSRect(x: frame.minX, y: frame.minY, width: frame.width + grow.width, height: frame.height + grow.height),
                display: false)
            nsWindow.contentView?.layoutSubtreeIfNeeded()
            stage.layoutInsetViews()
            let kept = stage.insetColumn.map { Self.near(Self.measuredAnchor($0, stage: stage.insetLayer.bounds), anchor) } == true
            results.append(kept && stage.insetAnchor == anchor)
        }
        nsWindow.setFrame(frame, display: false)
        nsWindow.contentView?.layoutSubtreeIfNeeded()
        print("inset drag: the column keeps its place when the window grows \(results[0]) and shrinks \(results[1])")
        if results.contains(false) { problems.append("resizing the window moved the inset column off its fraction") }
        insetDragRestoreStep(doorbell)
    }

    /// The position is saved with the window: windows.json's record reads
    /// back, a window restored from it puts its column there, and "Reset
    /// Position" in the inset's menu returns it to the default corner.
    func insetDragRestoreStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let saved = window.savedWindow, let anchor = window.stage.insetAnchor else {
            return insetDragMultiStep(doorbell)
        }
        // parity: inset-drag-restore
        let record = (try? JSONEncoder().encode([saved])).flatMap { try? JSONDecoder().decode([SavedWindow].self, from: $0) }
        let readsBack = record?.first?.insetAnchor == anchor
        guard let restored = app.openWindow(window.state.content) else {
            problems.append("could not open a window to restore into")
            return insetDragMultiStep(doorbell)
        }
        restored.restoreInsetAnchor(record?.first?.insetAnchor)
        // The new window adopts the open conversation (after a turn of the main queue).
        when({ !restored.stage.shownInsetViews.isEmpty }, timeout: 2) { [self] in
            restored.window?.contentView?.layoutSubtreeIfNeeded()
            restored.stage.layoutInsetViews()
            let there = restored.stage.insetAnchor == anchor
                && restored.stage.insetColumn.map { Self.near(Self.measuredAnchor($0, stage: restored.stage.insetLayer.bounds), anchor) } == true
            restored.close()
            // Reset Position, from the inset's own menu.
            let stage = window.stage
            var reset = false
            if let view = stage.shownInsetViews.first, let nsWindow = window.window,
                let e = NSEvent.mouseEvent(
                    with: .rightMouseDown, location: view.convert(CGPoint(x: 20, y: 8), to: nil), modifierFlags: [],
                    timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: nsWindow.windowNumber, context: nil,
                    eventNumber: 0, clickCount: 1, pressure: 1),
                let menu = view.menu(for: e), let item = menu.items.firstIndex(where: { $0.title == "Reset Position" })
            {
                menu.performActionForItem(at: item)
                reset = stage.insetAnchor == nil
                    && stage.insetColumn.map { Self.near(Self.measuredAnchor($0, stage: stage.insetLayer.bounds), InsetAnchor(corner: model.insetCorner)) } == true
                    && window.savedWindow?.insetAnchor == nil
            }
            print("inset drag: saved record reads back \(readsBack), a restored window puts the column there \(there), Reset Position \(reset)")
            if !readsBack { problems.append("the window's saved record lost the inset position") }
            if !there { problems.append("a window restored from the record did not put its insets there") }
            if !reset { problems.append("Reset Position did not return the column to the default corner") }
            insetDragMultiStep(doorbell)
        }
    }

    /// Several insets move as one column; at the bottom the newest is at
    /// the bottom and the others stack upward, at the top the reverse.
    func insetDragMultiStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let device = model.runtime("fake-1"),
            let id = device.deviceID
        else { return smokeStep() }
        let stage = window.stage
        let hall = CameraRef(device: device.key, channel: 4)
        // parity: inset-drag-multi
        // A smoke alarm's inset (it stays after the stop) beside the conversation's.
        fake.emitSoundDetection(device: id, channel: 4, start: true)
        fake.emitSoundDetection(device: id, channel: 4, start: false)
        when({ stage.shownInsetViews.count == 2 }, timeout: 3) { [self] in
            guard let header = stage.shownInsetViews.first?.frame, let start = stage.insetColumn else {
                problems.append("a second inset did not open")
                return finishInsetDrag(doorbell, hall)
            }
            let b = stage.insetLayer.bounds
            @MainActor func column() -> [CGRect] { stage.shownInsetViews.map { $0.frame } }
            // To the bottom left (both axes snap).
            let grab = CGPoint(x: header.minX + 40, y: header.minY + 8)
            let bottomLeft = CGPoint(x: 15, y: b.height - start.height - 15)
            drag(from: grab, to: CGPoint(x: grab.x + bottomLeft.x - start.minX, y: grab.y + bottomLeft.y - start.minY), in: window)
            let low = column()
            let m = CGFloat(InsetGeometry.margin), gap = CGFloat(InsetGeometry.gap)
            let upward = stage.insetAnchor == InsetAnchor(x: 0, y: 1) && low.count == 2
                && low.allSatisfy { $0.minX == m } && abs(low[0].maxY - (b.height - m)) < 0.5
                && abs(low[1].maxY - (low[0].minY - gap)) < 0.5
            // To the top, a third of the way across (x does not snap).
            guard let newest = stage.shownInsetViews.first?.frame, let now = stage.insetColumn else {
                return finishInsetDrag(doorbell, hall)
            }
            let grab2 = CGPoint(x: newest.minX + 40, y: newest.minY + 8)
            let topThird = CGPoint(x: m + (b.width - 2 * m - now.width) / 3, y: 20)
            drag(from: grab2, to: CGPoint(x: grab2.x + topThird.x - now.minX, y: grab2.y + topThird.y - now.minY), in: window)
            let high = column()
            let downward = stage.insetAnchor.map { $0.y == 0 && abs($0.x - 1.0 / 3) < 0.01 } == true && high.count == 2
                && high[0].minX == high[1].minX && abs(high[0].minY - m) < 0.5 && abs(high[1].minY - (high[0].maxY + gap)) < 0.5
            print("inset drag with two insets: one column, bottom grows upward \(upward), top grows downward \(downward)")
            if !upward { problems.append("two insets at the bottom did not stack upward from it in one column: \(low)") }
            if !downward { problems.append("two insets at the top did not stack downward from it in one column: \(high)") }
            finishInsetDrag(doorbell, hall)
        }
    }

    func finishInsetDrag(_ doorbell: CameraRef, _ hall: CameraRef) {
        guard let window else { return smokeStep() }
        let stage = window.stage
        stage.dismissInset(hall)
        stage.endConversation(doorbell)
        stage.resetInsetPosition()
        smokeStep()
    }

    /// A smoke alarm's inset stays until dismissed; dismissing it dismisses
    /// the alert.
    func smokeStep() {
        guard let model = app.model, let window, let fake = model.fake, let device = model.runtime("fake-1"),
            let id = device.deviceID
        else { return checkLog() }
        let stage = window.stage
        let hall = CameraRef(device: device.key, channel: 4)
        // parity: alert-inset-smoke-stays
        fake.emitSoundDetection(device: id, channel: 4, start: true)
        fake.emitSoundDetection(device: id, channel: 4, start: false)
        when({ stage.insetBoard.item(for: hall)?.active == false }, timeout: 3) { [self] in
            let critical = stage.insetBoard.item(for: hall)?.kind.isCritical == true
            advance(InsetBoard.lostStopAfter + InsetBoard.autoCloseDelay + 60)
            let stays = stage.insetBoard.item(for: hall) != nil
            print("smoke inset: critical \(critical), still open after 4 min \(stays), alert \(model.alerts.isActive(hall))")
            if !critical || !stays { problems.append("the smoke alarm's inset did not stay") }
            press("\u{1b}", keyCode: ViewerKeys.escape)
            print("smoke inset dismissed \(stage.insetBoard.isEmpty), alert dismissed \(!model.alerts.isActive(hall))")
            if !stage.insetBoard.isEmpty { problems.append("Esc did not dismiss the smoke alarm's inset") }
            if model.alerts.isActive(hall) { problems.append("dismissing the smoke inset did not dismiss its alert") }
            stage.unfocus()
            advance(StageView.focusDuration)
            fisheyeStep()
        }
    }

    // MARK: Fisheye (docs/UX.md "Fisheye cameras")
}
