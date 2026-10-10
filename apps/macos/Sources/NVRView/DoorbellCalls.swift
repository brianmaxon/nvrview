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
/// user allowed it for that doorbell in Settings (the core refuses
/// otherwise), and never started on its own.
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
        var tapped: CoreTile?
        /// A status note for the call (microphone refused, ...).
        var note: String?

        init(call: DoorbellCall) { self.call = call }
    }

    private unowned let model: AppModel
    private let services: CallServices
    private var sessions: [CameraRef: Session] = [:]
    private let taps: TapRouter

    init(model: AppModel, services: CallServices, taps: TapRouter) {
        self.model = model
        self.services = services
        self.taps = taps
    }

    /// The call for a doorbell (any lens of its channel), if one is open.
    func call(for camera: CameraRef) -> DoorbellCall? { sessions[camera.firstLens]?.call }
    var cameras: [CameraRef] { Array(sessions.keys) }
    var isEmpty: Bool { sessions.isEmpty }
    func note(for camera: CameraRef) -> String? { sessions[camera.firstLens]?.note }

    /// The fake microphone, when the services are fakes (self-test).
    var fakeMicrophone: FakeMicrophone? { services.microphone as? FakeMicrophone }
    var fakeSpeech: FakeSpeechRenderer? { services.speech as? FakeSpeechRenderer }

    // MARK: Ring

    /// A doorbell press (`visitor` alarm start).
    func ring(_ doorbell: CameraRef, at date: Date) {
        let camera = doorbell.firstLens
        let now = model.clock.now
        if let s = sessions[camera] {
            s.call.pressed(now: now)
            model.logEvent("Doorbell pressed again: channel \(camera.channel + 1)")
            model.changed()
            return
        }
        let decision = CallAudio.decide(
            doorbell: camera, listening: model.audible, unmuteOnRing: model.cameraOptions(camera).unmuteOnRing)
        let s = Session(call: DoorbellCall(camera: camera, startedAt: date, now: now, audio: decision))
        sessions[camera] = s
        model.leases.acquire(camera)
        if case .audible = decision {
            // Audible at once: the cameras that were playing are muted for the call.
            model.setAudible([camera])
        }
        startVisitorTranscript(s)
        tileChanged(camera)
        model.logEvent("Doorbell call: channel \(camera.channel + 1), \(decision == .overlay ? "muted (Unmute shown)" : "audible")")
        model.changed()
    }

    /// The doorbell's tile opened or closed: its tap follows.
    func tileChanged(_ camera: CameraRef) {
        guard let s = sessions[camera.firstLens] else { return }
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
        if final { s.call.transcript.final(text, source: source) } else { s.call.transcript.partial(text, source: source) }
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

    /// Talk pressed (held, or the toggle on): starts talk-back with the
    /// microphone. Nothing is sent without the doorbell's permission: the
    /// core refuses with `not_permitted`.
    func startTalking(_ camera: CameraRef) {
        guard let s = sessions[camera.firstLens], !s.micTalk else { return }
        s.micTalk = true
        s.call.activity(now: model.clock.now)
        services.microphone.requestAccess { [weak self] granted, reason in
            guard let self, let s = self.sessions[camera.firstLens], s.micTalk else { return }
            guard granted else {
                s.micTalk = false
                s.note = "Talk-back: " + (reason ?? "no microphone")
                self.model.changed()
                return
            }
            s.note = nil
            guard self.beginTalk(s) else {
                s.micTalk = false
                self.model.changed()
                return
            }
            self.startMicrophone(s)
            self.model.changed()
        }
    }

    /// Talk released (or the toggle off).
    func stopTalking(_ camera: CameraRef) {
        guard let s = sessions[camera.firstLens], s.micTalk else { return }
        s.micTalk = false
        services.microphone.stop()
        s.you?.finish()
        s.you = nil
        s.call.activity(now: model.clock.now)
        if s.pendingReply == nil, let tile = model.leases.tile(for: s.call.camera) {
            try? model.bridge.stopTalk(tile, drain: false)
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
        do throws(CoreStatus) {
            try model.bridge.startTalk(tile)
            s.call.talk.startSent()
            model.logEvent("Talk-back started: channel \(s.call.camera.channel + 1)")
            return true
        } catch {
            s.call.talk.startFailed(error)
            model.logEvent("Talk-back refused: channel \(s.call.camera.channel + 1), \(TalkState.words(error == .notPermitted ? "not_permitted" : "error"))")
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
        do {
            try services.microphone.start { samples in
                try? bridge.pushTalk(tile, samples: samples, sampleRate: 16_000)
                // Not while the doorbell plays through the speakers (echo).
                let peak = bridge.audioLevel(tile)?.peak ?? 0
                if echo.allows(now: ProcessInfo.processInfo.systemUptime, peak: peak) { you.feed(pcm16k: samples) }
            }
        } catch {
            s.note = "Talk-back: the microphone could not start"
            stopTalking(s.call.camera)
        }
    }

    /// Send: speaks `text` (what the user typed in the call, nothing else)
    /// through the doorbell with the system voice, under the same permission
    /// and limits as Talk.
    func sendReply(_ text: String, to camera: CameraRef) {
        let typed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let s = sessions[camera.firstLens], !typed.isEmpty else { return }
        s.call.activity(now: model.clock.now)
        model.logEvent("Typed reply sent: channel \(s.call.camera.channel + 1), \(typed.count) characters")
        let speech = services.speech
        Task { @MainActor [weak self] in
            let samples = await speech.render(typed)
            guard let self, let s = self.sessions[camera.firstLens], !samples.isEmpty else { return }
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
            s.pendingReply = nil
            if s.micTalk {
                s.micTalk = false
                services.microphone.stop()
                s.you?.finish()
                s.you = nil
            }
            if e.state == .refused {
                model.logEvent("Talk-back refused: channel \(camera.channel + 1), \(TalkState.words(e.reason ?? "refused", retryMilliseconds: e.retryMilliseconds))")
            } else {
                model.logEvent("Talk-back stopped: channel \(camera.channel + 1), \(e.reason ?? "stopped"), \(e.sentMilliseconds ?? 0) ms sent")
            }
        default:
            break
        }
        model.changed()
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
        let text = s.call.logText(endedAt: Date(), saveTranscript: model.saveCallTranscripts)
        model.logEvent(text + " (\(reason))", transcriptFollows: model.saveCallTranscripts && !s.call.transcript.finals.isEmpty)
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
/// doorbell is audible is set from the main thread.
final class EchoBox: Sendable {
    private let gate = Mutex(EchoGate())
    private let audible = Atomic<Bool>(false)

    var doorbellAudible: Bool {
        get { audible.load(ordering: .relaxed) }
        set { audible.store(newValue, ordering: .relaxed) }
    }

    func allows(now: Double, peak: Float) -> Bool {
        let playing = doorbellAudible
        return gate.withLock { $0.allowsMicrophone(now: now, doorbellAudible: playing, doorbellPeak: peak) }
    }
}
