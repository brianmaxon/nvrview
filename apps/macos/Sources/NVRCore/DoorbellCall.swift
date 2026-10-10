import Foundation

// Doorbell calls (docs/UX.md "Doorbell calls"): a press opens the doorbell's
// inset as a call. The decisions are pure and tested headless: whether the
// doorbell becomes audible, where talk-back stands, the live transcript, and
// when the microphone's transcription is gated against the speakers' echo.

// MARK: Audio

/// Whether a ring makes the doorbell audible (docs/UX.md "Doorbell calls").
public enum CallAudio {
    public enum Decision: Sendable, Equatable {
        /// Audible at once; `muted` are the cameras that were playing (muted
        /// for the call, audible again after it).
        case audible(muted: Set<CameraRef>)
        /// Silent: a large Unmute control covers the inset (one click
        /// anywhere on it).
        case overlay
    }

    /// The user was listening to `listening` when it rang; `unmuteOnRing` is
    /// the doorbell's "Unmute on ring even when not listening".
    public static func decide(doorbell: CameraRef, listening: Set<CameraRef>, unmuteOnRing: Bool) -> Decision {
        let others = listening.subtracting([doorbell])
        if !listening.isEmpty { return .audible(muted: others) }
        return unmuteOnRing ? .audible(muted: []) : .overlay
    }
}

// MARK: Talk-back

/// Where the call's talk-back stands, from what the app asked and the core's
/// `talk` events. Talk-back is camera motion control: it starts only when the
/// user presses Talk (or sends a typed reply), never on its own.
public struct TalkState: Sendable, Equatable {
    public enum Phase: Sendable, Equatable {
        case idle
        /// Asked for; no event yet.
        case requested
        case starting
        case active
        case refused(reason: String, retryMilliseconds: Int?)
        case stopped(reason: String)
    }

    public private(set) var phase: Phase = .idle

    public init() {}

    /// Talk is asked for, starting or running.
    public var isTalking: Bool {
        switch phase {
        case .requested, .starting, .active: true
        default: false
        }
    }

    public var isActive: Bool { phase == .active }

    /// The user pressed Talk: true when the core should be asked to start
    /// (asking while one runs changes nothing).
    public mutating func requestStart() -> Bool {
        guard !isTalking else { return false }
        phase = .requested
        return true
    }

    /// `nv_talk_start` failed at once (`.notPermitted` without the permission).
    public mutating func startFailed(_ status: CoreStatus) {
        let reason = status == .notPermitted ? "not_permitted" : status == .unknownID ? "tile_closed" : "error"
        phase = .refused(reason: reason, retryMilliseconds: nil)
    }

    /// Folds in a `talk` event for the call's tile.
    public mutating func apply(_ event: TalkEvent) {
        switch event.state {
        case .starting: phase = .starting
        case .active: phase = .active
        case .refused:
            phase = .refused(reason: event.reason ?? "refused", retryMilliseconds: event.retryMilliseconds)
        case .stopped: phase = .stopped(reason: event.reason ?? "stopped")
        case .other: break
        }
    }

    /// The user let go of Talk: back to idle once the stop is asked for.
    public mutating func stopRequested() {
        if phase == .requested { phase = .idle }
    }

    /// The call's status line.
    public var text: String {
        switch phase {
        case .idle: "Hold Talk to speak"
        case .requested, .starting: "Connecting talk-back…"
        case .active: "Talking"
        case .refused(let reason, let retry): "Talk-back refused: " + Self.words(reason, retryMilliseconds: retry)
        case .stopped(let reason): reason == "stopped" || reason == "finished" ? "Talk ended" : "Talk ended: " + Self.words(reason)
        }
    }

    /// A refusal or stop reason in words.
    public static func words(_ reason: String, retryMilliseconds: Int? = nil) -> String {
        switch reason {
        case "not_permitted": return "not enabled for this doorbell (Settings > Cameras)"
        case "rate_limited":
            let s = retryMilliseconds.map { " (try again in \(($0 + 999) / 1000) s)" } ?? ""
            return "too many attempts\(s)"
        case "not_supported": return "this camera has no talk-back"
        case "unsupported_ability": return "this camera's talk-back format is not supported"
        case "busy": return "another client is talking"
        case "device_refused": return "the device refused"
        case "session_closed", "connection_lost": return "the connection was lost"
        case "timed_out": return "the device did not answer"
        case "permission_revoked": return "talk-back was turned off"
        case "idle": return "no audio for a while"
        case "max_duration": return "the longest talk allowed"
        case "tile_closed": return "the video closed"
        default: return reason.replacingOccurrences(of: "_", with: " ")
        }
    }
}

// MARK: Transcript

/// Who spoke: the doorbell's audio or this computer's microphone. One
/// transcription session per source, so the label is the source.
public enum TranscriptSource: String, Sendable, CaseIterable, Hashable {
    case visitor
    case you

    public var label: String {
        switch self {
        case .visitor: "Visitor"
        case .you: "You"
        }
    }
}

/// The call's live transcript: finished phrases in order, and per source the
/// phrase still being recognized (replaced by each new partial result, then
/// by the final one).
public struct Transcript: Sendable, Equatable {
    public struct Line: Sendable, Equatable {
        public var source: TranscriptSource
        public var text: String
        public var isFinal: Bool

        public init(source: TranscriptSource, text: String, isFinal: Bool) {
            self.source = source
            self.text = text
            self.isFinal = isFinal
        }
    }

    public private(set) var finals: [Line] = []
    public private(set) var partials: [TranscriptSource: String] = [:]

    public init() {}

    /// A volatile result: replaces the source's phrase in progress.
    public mutating func partial(_ text: String, source: TranscriptSource) {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        partials[source] = t.isEmpty ? nil : t
    }

    /// A final result: appended; the source's phrase in progress is done.
    public mutating func final(_ text: String, source: TranscriptSource) {
        partials[source] = nil
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        finals.append(Line(source: source, text: t, isFinal: true))
    }

    /// What the call shows: the finished lines, then the phrases in progress.
    public var lines: [Line] {
        finals + TranscriptSource.allCases.compactMap { s in partials[s].map { Line(source: s, text: $0, isFinal: false) } }
    }

    public var isEmpty: Bool { finals.isEmpty && partials.isEmpty }

    /// The saved form (only when the user turned saving on): finished lines,
    /// "Visitor: ..." and "You: ...".
    public var savedText: String {
        finals.map { "\($0.source.label): \($0.text)" }.joined(separator: "\n")
    }
}

/// Keeps the microphone's transcription from hearing the visitor twice: while
/// the doorbell's audio plays through the speakers and carries sound, and a
/// moment after, the microphone's samples are not transcribed.
public struct EchoGate: Sendable, Equatable {
    /// Peak level (0...1) above which the doorbell counts as playing.
    public var threshold: Float = 0.03
    /// How long after the doorbell goes quiet the microphone stays gated
    /// (room echo and the audio path's delay).
    public var hold: Double = 0.4
    private var gatedUntil: Double = -.infinity

    public init() {}

    /// Whether the microphone may be transcribed at `now`, given whether the
    /// doorbell is audible and its level.
    public mutating func allowsMicrophone(now: Double, doorbellAudible: Bool, doorbellPeak: Float) -> Bool {
        if doorbellAudible && doorbellPeak > threshold { gatedUntil = now + hold }
        return now >= gatedUntil
    }
}

// MARK: Transcription engines

/// A speech recognizer for one source (docs/ARCHITECTURE.md "Live
/// transcription"): Apple's SpeechAnalyzer on macOS behind this interface,
/// so another engine (Windows) can match it and tests can fake it.
///
/// `start` prepares (it may install the language's assets) and begins a
/// session; `feed` takes 16 kHz mono samples from any thread; `finish` ends
/// the session after what was fed. Results arrive through the callbacks, on
/// any thread.
public protocol TranscriptionEngine: AnyObject, Sendable {
    /// A volatile result: the phrase so far (replace the previous partial).
    var onPartial: (@Sendable (String) -> Void)? { get set }
    /// A final result for a phrase.
    var onFinal: (@Sendable (String) -> Void)? { get set }
    /// The engine cannot go on (no recognizer for the language, assets not
    /// installable, ...); the call continues without a transcript.
    var onError: (@Sendable (String) -> Void)? { get set }
    /// Progress while preparing ("preparing speech recognition").
    var onStatus: (@Sendable (TranscriptionStatus) -> Void)? { get set }

    func start(source: TranscriptSource)
    func feed(pcm16k: [Float])
    func finish()
}

/// Where a transcription session stands, for the call's status line.
public enum TranscriptionStatus: Sendable, Equatable {
    case preparing
    case running
    case unavailable(String)
    case finished
}

// MARK: The call

/// One doorbell call: from the press to the close or about a minute after the
/// last activity (a press, speech, talk-back).
public struct DoorbellCall: Sendable, Equatable {
    public static let idleTimeout = 60.0

    public var camera: CameraRef
    public var startedAt: Date
    /// When the last press, speech or talk happened, on the viewer clock.
    public var lastActivity: Double
    public var presses = 1
    /// The doorbell plays through the speakers.
    public var audible: Bool
    /// The Unmute control covers the video.
    public var showsUnmute: Bool
    /// Cameras muted for the call; audible again when it ends (unless the
    /// user changed what plays meanwhile).
    public var mutedForCall: Set<CameraRef>
    public var talk = TalkState()
    public var transcript = Transcript()
    public var transcription: TranscriptionStatus = .preparing

    public init(camera: CameraRef, startedAt: Date, now: Double, audio: CallAudio.Decision) {
        self.camera = camera
        self.startedAt = startedAt
        lastActivity = now
        switch audio {
        case .audible(let muted):
            audible = true
            showsUnmute = false
            mutedForCall = muted
        case .overlay:
            audible = false
            showsUnmute = true
            mutedForCall = []
        }
    }

    /// Activity keeps the call open.
    public mutating func activity(now: Double) { lastActivity = max(lastActivity, now) }

    /// Another press while the call is open.
    public mutating func pressed(now: Double) {
        presses += 1
        activity(now: now)
    }

    /// The Unmute control was clicked.
    public mutating func unmuted() {
        audible = true
        showsUnmute = false
    }

    /// Whether the call has ended by itself at `now` (talking keeps it open).
    public func expired(now: Double) -> Bool {
        !talk.isTalking && now - lastActivity >= Self.idleTimeout
    }

    /// The event log's line when it ends; the transcript only when saving is
    /// on.
    public func logText(endedAt: Date, saveTranscript: Bool) -> String {
        let seconds = Int(endedAt.timeIntervalSince(startedAt).rounded())
        var text = "Doorbell call ended: channel \(camera.channel + 1), \(presses) press\(presses == 1 ? "" : "es"), \(seconds) s"
        if saveTranscript, !transcript.finals.isEmpty { text += "\n" + transcript.savedText }
        return text
    }
}
