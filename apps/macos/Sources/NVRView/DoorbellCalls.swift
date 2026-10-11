import AppKit
import Foundation
import NVRCore
import Synchronization

/// The doorbell calls in progress (docs/UX.md "Doorbell calls"), shared by
/// every window: a press opens one; the windows show it in the doorbell's
/// inset (or over the stage when the doorbell fills it).
///
/// A call holds a lease on the doorbell (its tile stays open whatever the
/// windows show), taps its audio for the visitor's transcript, decides
/// whether it is audible, and runs talk-back only when the user presses Talk
/// or sends a typed reply: talk-back is camera motion control, off unless the
/// user allowed it for that camera (Settings, or the confirmation shown when
/// Talk or Send finds it off; the core refuses otherwise), and never started
/// on its own.
@MainActor
final class DoorbellCalls {
    /// One call's moving parts besides its pure state.
    private final class Session {
        var call: DoorbellCall
        var visitor: (any TranscriptionEngine)?
        var you: (any TranscriptionEngine)?
        /// Talk-back started by holding Talk or the toggle (the microphone).
        var micTalk = false
        /// A typed reply waiting for the talk to become active.
        var pendingReply: [Float]?
        /// The echo gate, run on the microphone's thread with the doorbell's
        /// level for each block (the level is lock-free to read).
        let echo = EchoBox()
        /// Whether the system cancels the doorbell's echo while the
        /// microphone is on (nil while it is off).
        var echoCancellation: EchoCancellation?
        var tapped: CoreTile?
        /// A status note for the call (microphone refused, ...).
        var note: String?
        /// The confirmation to allow talk-back is on screen.
        var asking = false
        /// What the user last started, and the typed reply's audio, so a
        /// refusal from the core can be answered by the same prompt.
        var lastAction: TalkPermission.Action?
        var lastReply: [Float]?
        /// The doorbell tile the core was told is in this conversation (its
        /// talk starts skip the per-minute limit while the call is open).
        var conversationTile: CoreTile?

        init(call: DoorbellCall) { self.call = call }
    }

    private unowned let model: AppModel
    private let services: CallServices
    private var sessions: [CameraRef: Session] = [:]
    private let taps: TapRouter
    private var echoLog = EchoCancellationLog()

    init(model: AppModel, services: CallServices, taps: TapRouter) {
        self.model = model
        self.services = services
        self.taps = taps
    }

    /// The call for a doorbell (any lens of its channel), if one is open.
    func call(for camera: CameraRef) -> DoorbellCall? { sessions[camera.firstLens]?.call }
    var cameras: [CameraRef] { Array(sessions.keys) }
    var isEmpty: Bool { sessions.isEmpty }
    /// The call's status note: a problem (microphone refused, ...), else
    /// whether echo cancellation is on while the microphone is. The Mac's
    /// call views show the note instead of the talk state, so it is joined
    /// to it here.
    func note(for camera: CameraRef) -> String? {
        guard let s = sessions[camera.firstLens] else { return nil }
        if let note = s.note { return note }
        guard s.micTalk, let echo = s.echoCancellation else { return nil }
        return s.call.talk.text + " · " + echo.note
    }

    /// Whether the system cancels the doorbell's echo in the microphone
    /// (nil while the microphone is off).
    func echoCancellation(for camera: CameraRef) -> EchoCancellation? {
        guard let s = sessions[camera.firstLens], s.micTalk else { return nil }
        return s.echoCancellation
    }

    /// Whether the caption gate runs in its echo-cancelled mode (self-test).
    func echoGateCancelled(_ camera: CameraRef) -> Bool? { sessions[camera.firstLens]?.echo.echoCancelled }

    /// The fake microphone, when the services are fakes (self-test).
    var fakeMicrophone: FakeMicrophone? { services.microphone as? FakeMicrophone }
    var fakePrompter: FakeTalkPrompter? { services.prompter as? FakeTalkPrompter }
    var fakeSpeech: FakeSpeechRenderer? { services.speech as? FakeSpeechRenderer }

    // MARK: Ring

    /// A doorbell press (`visitor` alarm start).
    func ring(_ doorbell: CameraRef, at date: Date) {
        let camera = doorbell.firstLens
        if let s = sessions[camera] {
            s.call.pressed(now: model.clock.now)
            model.logEvent("Doorbell pressed again: channel \(camera.channel + 1)")
            model.changed()
            return
        }
        let decision = CallAudio.decide(
            doorbell: camera, listening: model.audible, unmuteOnRing: model.cameraOptions(camera).unmuteOnRing)
        open(camera, at: date, audio: decision, origin: .press)
        model.logEvent("Doorbell call: channel \(camera.channel + 1), \(decision == .overlay ? "muted (Unmute shown)" : "audible")")
        model.changed()
    }

    /// "Start Conversation" on a camera's tile: the same call with no press,
    /// for captions, Talk and typed replies. Talk-back is still camera motion
    /// control: nothing is sent until the user presses Talk or sends a reply,
    /// and the core refuses without the camera's permission. A call already
    /// open on the camera is kept as it is.
    func startConversation(_ camera: CameraRef, at date: Date) {
        let camera = camera.firstLens
        if let s = sessions[camera] {
            s.call.activity(now: model.clock.now)
            model.changed()
            return
        }
        let decision = CallAudio.decideConversation(doorbell: camera, listening: model.audible)
        open(camera, at: date, audio: decision, origin: .conversation)
        model.logEvent("Conversation started: channel \(camera.channel + 1)")
        model.changed()
    }

    /// The user did something with the call's inset (hovered, clicked,
    /// typed): it counts as activity, so the call stays open.
    func use(_ camera: CameraRef) {
        sessions[camera.firstLens]?.call.activity(now: model.clock.now)
    }

    /// "Keep talking" turned on or off. Kept with the call, so every view of
    /// it (another window's inset, a view built again) shows the same.
    func setLatched(_ camera: CameraRef, _ on: Bool) {
        guard let s = sessions[camera.firstLens] else { return }
        s.call.setLatched(on, now: model.clock.now)
        if on { startTalking(camera) } else { stopTalking(camera) }
        model.changed()
    }

    /// The reply field's text, kept with the call until it is sent.
    func setDraft(_ camera: CameraRef, _ text: String) {
        sessions[camera.firstLens]?.call.edited(draft: text, now: model.clock.now)
    }

    /// Pop Out (`.window`) or Pop In (`.inset`): one state for the whole app
    /// (docs/UX.md "A call in its own window"). Nothing else about the call
    /// changes (lease, tap, transcript, talk, latch, draft); the pop counts
    /// as activity, so the call gets a fresh minute.
    func setPresentation(_ camera: CameraRef, _ presentation: CallPresentation) {
        guard let s = sessions[camera.firstLens], s.call.presentation != presentation else { return }
        s.call.presentation = presentation
        s.call.activity(now: model.clock.now)
        model.changed()
    }

    /// The doorbell's audio is tapped for the visitor's transcript (self-test).
    func isTapped(_ camera: CameraRef) -> Bool { sessions[camera.firstLens]?.tapped != nil }

    /// A device was removed: its calls end.
    func endCalls(device key: String) {
        for camera in Array(sessions.keys) where camera.device == key { end(camera, reason: "device removed") }
    }

    /// Opens the call: lease, audio, and the visitor's transcript. Where it
    /// shows is decided now (the doorbell's choice, else "Open calls in
    /// their own window") and kept for presses that join it.
    private func open(_ camera: CameraRef, at date: Date, audio: CallAudio.Decision, origin: DoorbellCall.Origin) {
        let s = Session(
            call: DoorbellCall(
                camera: camera, startedAt: date, now: model.clock.now, audio: audio, origin: origin,
                presentation: model.callPresentation(for: camera)))
        sessions[camera] = s
        // Echo cancellation's slow setup, ahead of Talk (no microphone yet).
        services.microphone.prepare(true)
        model.leases.acquire(camera)
        if case .audible = audio {
            // Audible at once: the cameras that were playing are muted for the call.
            model.setAudible([camera])
        }
        startVisitorTranscript(s)
        tileChanged(camera)
    }

    /// Tells the core the call is open on the doorbell's current tile, so
    /// its talk starts are not limited per minute (the core clears the mark
    /// when the tile closes or the permission is withdrawn; marking again is
    /// harmless).
    private func markConversation(_ s: Session) {
        let tile = model.leases.tile(for: s.call.camera)
        if let old = s.conversationTile, old !== tile {
            try? model.bridge.setTalkConversation(old, open: false)
            s.conversationTile = nil
        }
        guard let tile else { return }
        if (try? model.bridge.setTalkConversation(tile, open: true)) != nil { s.conversationTile = tile }
    }

    /// The call ended: its tile is no longer in a conversation.
    private func unmarkConversation(_ s: Session) {
        guard let tile = s.conversationTile else { return }
        s.conversationTile = nil
        try? model.bridge.setTalkConversation(tile, open: false)
    }

    /// The doorbell tile the core was told is in a conversation (self-test).
    func conversationTile(_ camera: CameraRef) -> CoreTile? { sessions[camera.firstLens]?.conversationTile }

    /// The doorbell's tile opened or closed: its tap and the conversation
    /// mark follow.
    func tileChanged(_ camera: CameraRef) {
        guard let s = sessions[camera.firstLens] else { return }
        markConversation(s)
        let tile = model.leases.tile(for: camera.firstLens)
        if let old = s.tapped, old !== tile {
            taps.remove(old.id)
            s.tapped = nil
        }
        guard let tile, s.tapped == nil, let visitor = s.visitor else { return }
        taps.add(tile.id, visitor)
        if (try? model.bridge.setTileAudioTap(tile, enabled: true)) != nil {
            s.tapped = tile
        } else {
            taps.remove(tile.id)
        }
    }

    private func startVisitorTranscript(_ s: Session) {
        let engine = makeEngine(.visitor, s)
        s.visitor = engine
        engine.start(source: .visitor)
    }

    private func makeEngine(_ source: TranscriptSource, _ s: Session) -> any TranscriptionEngine {
        let engine = services.makeTranscriber(source)
        let camera = s.call.camera
        engine.onPartial = { [weak self] text in
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.transcribed(camera, source, text, final: false) }
            }
        }
        engine.onFinal = { [weak self] text in
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.transcribed(camera, source, text, final: true) }
            }
        }
        engine.onStatus = { [weak self] status in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self, let s = self.sessions[camera], source == .visitor else { return }
                    s.call.transcription = status
                    self.model.changed()
                }
            }
        }
        engine.onError = { _ in }
        return engine
    }

    private func transcribed(_ camera: CameraRef, _ source: TranscriptSource, _ text: String, final: Bool) {
        guard let s = sessions[camera] else { return }
        if final {
            s.call.transcript.final(text, source: source, at: Date())
        } else {
            s.call.transcript.partial(text, source: source)
        }
        // Speech is activity: the call stays open.
        s.call.activity(now: model.clock.now)
        model.changed()
    }

    // MARK: Audio

    /// The large Unmute control: the doorbell plays; whatever played is
    /// muted for the call.
    func unmute(_ camera: CameraRef) {
        guard let s = sessions[camera.firstLens] else { return }
        let others = model.audible.subtracting([s.call.camera])
        s.call.mutedForCall.formUnion(others)
        s.call.unmuted()
        s.call.activity(now: model.clock.now)
        model.setAudible([s.call.camera])
        model.changed()
    }

    // MARK: Talk-back

    /// Whether the camera's own option allows talk-back.
    private func talkAllowed(_ camera: CameraRef) -> Bool { model.cameraOptions(camera.firstLens).allowTalk }

    /// The confirmation "Allow talk-back for <camera>?". Allow persists the
    /// option as the Settings toggle does (which also tells the core) and
    /// then runs `onAllow`; Cancel changes nothing. One prompt at a time.
    private func askTalkPermission(_ s: Session, action: TalkPermission.Action, onAllow: @escaping () -> Void, onCancel: @escaping () -> Void = {}) {
        guard !s.asking else { return onCancel() }
        s.asking = true
        let camera = s.call.camera
        let prompt = TalkPermission.prompt(cameraName: model.name(of: camera))
        services.prompter.ask(prompt) { [weak self] allow in
            guard let self else { return }
            s.asking = false
            let outcome = TalkPermission.outcome(allow: allow, action: action)
            if outcome.allowTalkBack { self.model.setAllowTalk(camera, true) }
            if outcome.continueAction, self.sessions[camera] === s { onAllow() } else { onCancel() }
            self.model.changed()
        }
    }

    /// Talk pressed (held, or the toggle on): starts talk-back with the
    /// microphone. When the camera's talk-back is off the user is asked
    /// first (nothing is sent before the answer); a `not_permitted` refusal
    /// from the core (options out of sync) asks the same way.
    func startTalking(_ camera: CameraRef) {
        guard let s = sessions[camera.firstLens], !s.micTalk, !s.asking else { return }
        s.call.activity(now: model.clock.now)
        s.lastAction = .microphone
        if TalkPermission.gate(allowed: talkAllowed(camera)) == .ask {
            askTalkPermission(
                s, action: .microphone, onAllow: { [weak self] in self?.beginMicrophoneTalk(s) },
                onCancel: { s.call.microphoneStopped() })
            return
        }
        beginMicrophoneTalk(s)
    }

    private func beginMicrophoneTalk(_ s: Session) {
        let camera = s.call.camera
        guard !s.micTalk else { return }
        s.micTalk = true
        services.microphone.requestAccess { [weak self] granted, reason in
            guard let self, let s = self.sessions[camera.firstLens], s.micTalk else { return }
            guard granted else {
                s.micTalk = false
                s.call.microphoneStopped()
                s.note = "Talk-back: " + (reason ?? "no microphone")
                self.model.changed()
                return
            }
            s.note = nil
            guard self.beginTalk(s) else {
                s.micTalk = false
                s.call.microphoneStopped()
                self.model.changed()
                return
            }
            self.startMicrophone(s)
            self.model.changed()
        }
    }

    /// Talk released (or the toggle off).
    func stopTalking(_ camera: CameraRef) {
        guard let s = sessions[camera.firstLens] else { return }
        // Whatever stops the microphone turns "Keep talking" off.
        s.call.microphoneStopped()
        guard s.micTalk else { return }
        s.micTalk = false
        services.microphone.stop()
        s.echoCancellation = nil
        s.you?.finish()
        s.you = nil
        s.call.activity(now: model.clock.now)
        // Drains: what the microphone pushed goes out, then the core's
        // silent tail, so the doorbell plays the last words before the talk
        // closes (talking again meanwhile continues the same talk).
        if s.pendingReply == nil, let tile = model.leases.tile(for: s.call.camera) {
            try? model.bridge.stopTalk(tile, drain: true)
        }
        s.call.talk.stopRequested()
        model.changed()
    }

    func isTalkingWithMicrophone(_ camera: CameraRef) -> Bool { sessions[camera.firstLens]?.micTalk == true }

    /// Asks the core for talk-back unless it runs already; false if refused
    /// at once.
    private func beginTalk(_ s: Session) -> Bool {
        guard let tile = model.leases.tile(for: s.call.camera) else {
            s.call.talk.startFailed(.unknownID)
            return false
        }
        guard s.call.talk.requestStart() else { return true }
        // Again before each start: withdrawing and allowing the permission
        // during the call cleared the core's mark.
        markConversation(s)
        do throws(CoreStatus) {
            try model.bridge.startTalk(tile)
            s.call.talk.startSent()
            model.logEvent("Talk-back started: \(model.name(of: s.call.camera))")
            return true
        } catch {
            s.call.talk.startFailed(error)
            model.logEvent(TalkPermission.refusal(cameraName: model.name(of: s.call.camera), reason: error == .notPermitted ? "not_permitted" : "error"))
            return false
        }
    }

    private func startMicrophone(_ s: Session) {
        guard let tile = model.leases.tile(for: s.call.camera) else { return }
        let you = makeEngine(.you, s)
        s.you = you
        you.start(source: .you)
        let bridge = model.bridge
        let echo = s.echo
        echo.doorbellAudible = model.audible.contains(s.call.camera)
        let camera = s.call.camera
        do {
            try services.microphone.start(
                { samples in
                    // Always sent (echo-cancelled when the system can).
                    try? bridge.pushTalk(tile, samples: samples, sampleRate: 16_000)
                    // Captioned unless the doorbell's echo may be in it
                    // (MicrophoneEchoGate).
                    let peak = bridge.audioLevel(tile)?.peak ?? 0
                    if echo.allows(now: ProcessInfo.processInfo.systemUptime, peak: peak) { you.feed(pcm16k: samples) }
                },
                onEcho: { [weak self] state in
                    echo.echoCancelled = state.isActive
                    DispatchQueue.main.async {
                        MainActor.assumeIsolated { self?.echoChanged(camera, state) }
                    }
                })
        } catch {
            s.note = "Talk-back: the microphone could not start"
            stopTalking(s.call.camera)
        }
    }

    /// The microphone said whether the system cancels the doorbell's echo:
    /// shown in the call, and logged once per outcome.
    private func echoChanged(_ camera: CameraRef, _ state: EchoCancellation) {
        guard let s = sessions[camera.firstLens], s.micTalk else { return }
        s.echoCancellation = state
        if let line = echoLog.line(for: state) { model.logEvent(line) }
        model.changed()
    }

    /// Send: speaks `text` (what the user typed in the call, nothing else)
    /// through the doorbell with the system voice, under the same permission
    /// and limits as Talk. `accepted(false)` when nothing was sent (the user
    /// answered Cancel to the talk-back prompt): the caller keeps the text.
    func sendReply(_ text: String, to camera: CameraRef, accepted done: @escaping (Bool) -> Void = { _ in }) {
        let typed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let s = sessions[camera.firstLens], !typed.isEmpty, !s.asking else { return done(false) }
        // On its way: the call's draft empties too (unless the user typed on).
        let accepted: (Bool) -> Void = { sent in
            if sent { s.call.sent(text) }
            done(sent)
        }
        s.call.activity(now: model.clock.now)
        s.lastAction = .reply
        if TalkPermission.gate(allowed: talkAllowed(camera)) == .ask {
            // Asked before anything is rendered or sent. Cancel drops
            // nothing: the text stays in the field (`accepted(false)`).
            askTalkPermission(
                s, action: .reply,
                onAllow: { [weak self] in
                    accepted(true)
                    self?.speakReply(typed, s)
                }, onCancel: { accepted(false) })
            return
        }
        accepted(true)
        speakReply(typed, s)
    }

    private func speakReply(_ typed: String, _ s: Session) {
        let camera = s.call.camera
        model.logEvent("Typed reply sent: \(model.name(of: camera)), \(typed.count) characters")
        let speech = services.speech
        Task { @MainActor [weak self] in
            let samples = await speech.render(typed)
            guard let self, let s = self.sessions[camera], !samples.isEmpty else { return }
            s.lastReply = samples
            s.pendingReply = (s.pendingReply ?? []) + samples
            if s.call.talk.isActive {
                self.flushReply(s)
            } else if !self.beginTalk(s) {
                s.pendingReply = nil
            }
            self.model.changed()
        }
    }

    private func flushReply(_ s: Session) {
        guard let samples = s.pendingReply, let tile = model.leases.tile(for: s.call.camera) else { return }
        s.pendingReply = nil
        try? model.bridge.pushTalk(tile, samples: samples, sampleRate: 16_000)
        // Ends after what was pushed has been sent, unless Talk is held.
        if !s.micTalk { try? model.bridge.stopTalk(tile, drain: true) }
    }

    /// A `talk` event for a tile.
    func talkEvent(_ e: TalkEvent) {
        guard let camera = model.leases.camera(forTile: e.tile), let s = sessions[camera.firstLens] else { return }
        // The events of an earlier start that arrive late change nothing.
        guard s.call.talk.apply(e) else { return }
        s.call.activity(now: model.clock.now)
        switch e.state {
        case .active:
            flushReply(s)
        case .refused, .stopped:
            let samples = s.lastReply
            s.pendingReply = nil
            let wasMic = s.micTalk
            s.call.microphoneStopped()
            if s.micTalk {
                s.micTalk = false
                services.microphone.stop()
                s.echoCancellation = nil
                s.you?.finish()
                s.you = nil
            }
            if e.state == .refused {
                model.logEvent(TalkPermission.refusal(cameraName: model.name(of: camera), reason: e.reason ?? "refused", retryMilliseconds: e.retryMilliseconds))
                if e.reason == "not_permitted" { coreRefusedPermission(s, wasMic: wasMic, reply: samples) }
            } else {
                model.logEvent("Talk-back stopped: \(model.name(of: camera)), \(e.reason ?? "stopped"), \(e.sentMilliseconds ?? 0) ms sent")
            }
        default:
            break
        }
        model.changed()
    }

    /// The core refused for lack of permission although the camera's option
    /// is on (out of sync) or was off: the same prompt; Allow tells the core
    /// again and repeats what the user asked.
    private func coreRefusedPermission(_ s: Session, wasMic: Bool, reply: [Float]?) {
        let action = wasMic ? TalkPermission.Action.microphone : (s.lastAction ?? .microphone)
        askTalkPermission(s, action: action) { [weak self] in
            guard let self else { return }
            if action == .reply, let reply {
                s.pendingReply = reply
                if !self.beginTalk(s) { s.pendingReply = nil }
            } else {
                self.beginMicrophoneTalk(s)
            }
        }
    }

    // MARK: Time

    /// Ends calls a minute after their last activity (the one-second timer;
    /// the self-test's clock).
    func tick(now: Double) {
        for (camera, s) in sessions where s.call.expired(now: now) { end(camera, reason: "no activity") }
    }

    /// What plays changed: the echo gate needs to know whether the doorbell
    /// is audible.
    func audibleChanged(_ audible: Set<CameraRef>) {
        for (camera, s) in sessions { s.echo.doorbellAudible = audible.contains(camera) }
    }

    /// Ends a call (closed, timed out, or the device went away): talk-back
    /// stops, the tap and transcription end, the audio goes back to what
    /// played before, and the call is logged (with its transcript only when
    /// the user turned saving on).
    func end(_ camera: CameraRef, reason: String) {
        guard let s = sessions.removeValue(forKey: camera.firstLens) else { return }
        if s.micTalk { services.microphone.stop() }
        if let tile = model.leases.tile(for: s.call.camera), s.call.talk.isTalking || s.pendingReply != nil {
            try? model.bridge.stopTalk(tile, drain: false)
        }
        unmarkConversation(s)
        if let tapped = s.tapped {
            taps.remove(tapped.id)
            try? model.bridge.setTileAudioTap(tapped, enabled: false)
        }
        s.visitor?.finish()
        s.you?.finish()
        // Back to what played before, if the user did not change it meanwhile.
        if model.audible == [s.call.camera] {
            model.setAudible(s.call.mutedForCall)
        }
        model.leases.release(s.call.camera)
        if sessions.isEmpty { services.microphone.prepare(false) }
        let endedAt = Date()
        let text = s.call.logText(endedAt: endedAt, saveTranscript: model.saveCallTranscripts)
        model.logEvent(text + " (\(reason))", transcriptFollows: model.saveCallTranscripts && !s.call.transcript.finals.isEmpty)
        // And as a text file of its own (nothing when nothing was transcribed).
        if model.saveCallTranscripts,
            let file = s.call.transcriptFile(cameraName: model.name(of: s.call.camera), endedAt: endedAt)
        {
            if let url = model.transcripts.save(name: file.name, text: file.text) {
                model.logEvent("Transcript saved: \(url.lastPathComponent)")
            } else {
                model.logEvent("Transcript not saved: the Transcripts folder could not be written")
            }
        }
        model.changed()
    }

    /// Ends every call (shutdown).
    func endAll() {
        for camera in Array(sessions.keys) { end(camera, reason: "closed") }
    }
}

/// Routes tapped audio (on the core's tap thread) to the transcription
/// engine of the call whose doorbell tile it is.
final class TapRouter: Sendable {
    private let routes = Mutex<[UInt64: any TranscriptionEngine]>([:])

    func add(_ tile: UInt64, _ engine: any TranscriptionEngine) { routes.withLock { $0[tile] = engine } }
    func remove(_ tile: UInt64) { routes.withLock { _ = $0.removeValue(forKey: tile) } }

    /// From the core's tap thread: 16 kHz mono, borrowed for the call.
    func deliver(_ block: CoreAudioBlock, tile: UInt64) {
        guard let engine = routes.withLock({ $0[tile] }) else { return }
        engine.feed(pcm16k: Array(block.samples))
    }
}

/// The echo gate for one call, used on the microphone's thread; whether the
/// doorbell is audible is set from the main thread, whether the system
/// cancels its echo from the microphone's.
final class EchoBox: Sendable {
    private let gate = Mutex(MicrophoneEchoGate())
    private let audible = Atomic<Bool>(false)
    private let cancelled = Atomic<Bool>(false)

    var doorbellAudible: Bool {
        get { audible.load(ordering: .relaxed) }
        set { audible.store(newValue, ordering: .relaxed) }
    }

    var echoCancelled: Bool {
        get { cancelled.load(ordering: .relaxed) }
        set { cancelled.store(newValue, ordering: .relaxed) }
    }

    func allows(now: Double, peak: Float) -> Bool {
        let playing = doorbellAudible
        let cancelled = echoCancelled
        return gate.withLock {
            $0.allowsMicrophone(now: now, doorbellAudible: playing, doorbellPeak: peak, echoCancelled: cancelled)
        }
    }
}
