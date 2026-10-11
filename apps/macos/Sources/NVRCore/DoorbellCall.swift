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

    /// The user started the conversation themselves (no ring): the doorbell
    /// plays, and whatever else was playing is muted for the call.
    public static func decideConversation(doorbell: CameraRef, listening: Set<CameraRef>) -> Decision {
        .audible(muted: listening.subtracting([doorbell]))
    }
}

// MARK: Talk-back permission

/// The confirmation shown when the user presses Talk or sends a typed reply
/// on a camera whose talk-back is off (docs/UX.md "Doorbell calls"). Talk-back
/// is camera motion control: it is allowed only by the user's own choice, per
/// camera, never by a ring and never without being asked.
public enum TalkPermission {
    /// What the user asked for.
    public enum Action: Sendable, Equatable {
        /// Talk with the microphone (held, or the toggle).
        case microphone
        /// A typed reply, spoken with the system voice.
        case reply
    }

    public enum Gate: Sendable, Equatable {
        /// The camera allows talk-back: go on.
        case proceed
        /// Ask first; nothing is sent until the user answers.
        case ask
    }

    /// Decided before the core is asked, from the camera's own option.
    public static func gate(allowed: Bool) -> Gate { allowed ? .proceed : .ask }

    /// What happens after the user answers the prompt.
    public struct Outcome: Sendable, Equatable {
        /// Persist the camera's allow-talk-back option (as the Settings
        /// toggle does, which also tells the core).
        public var allowTalkBack: Bool
        /// Carry on with the action the user started.
        public var continueAction: Bool
        /// A typed reply stays in the field to send later (Cancel), instead
        /// of being cleared.
        public var keepTypedText: Bool

        public init(allowTalkBack: Bool, continueAction: Bool, keepTypedText: Bool) {
            self.allowTalkBack = allowTalkBack
            self.continueAction = continueAction
            self.keepTypedText = keepTypedText
        }
    }

    public static func outcome(allow: Bool, action: Action) -> Outcome {
        Outcome(allowTalkBack: allow, continueAction: allow, keepTypedText: !allow && action == .reply)
    }

    /// The prompt's words.
    public struct Prompt: Sendable, Equatable {
        public var title: String
        public var message: String
        public var allow: String
        public var cancel: String
    }

    public static func prompt(cameraName: String) -> Prompt {
        Prompt(
            title: "Allow talk-back for \(cameraName)?",
            message: "Talk-back sends sound through this camera's speaker. You can turn it off in Settings.",
            allow: "Allow", cancel: "Cancel")
    }

    /// A refused talk-back in words, naming the camera (log lines).
    public static func refusal(cameraName: String, reason: String, retryMilliseconds: Int? = nil) -> String {
        "Talk-back refused: \(cameraName), " + TalkState.words(reason, retryMilliseconds: retryMilliseconds)
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
    /// Starts sent to the core whose `refused` or `stopped` has not arrived
    /// (each start ends with exactly one). The events of an earlier start
    /// that arrive late change nothing.
    public private(set) var outstanding = 0

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

    /// `nv_talk_start` was called and accepted: its events will follow.
    public mutating func startSent() { outstanding += 1 }

    /// `nv_talk_start` failed at once (`.notPermitted` without the
    /// permission; that refusal still sends its event, counted here).
    public mutating func startFailed(_ status: CoreStatus) {
        if status == .notPermitted { outstanding += 1 }
        let reason = status == .notPermitted ? "not_permitted" : status == .unknownID ? "tile_closed" : "error"
        phase = .refused(reason: reason, retryMilliseconds: nil)
    }

    /// Folds in a `talk` event for the call's tile. Returns false for an
    /// event that belongs to an earlier start (or to none): ignored.
    @discardableResult
    public mutating func apply(_ event: TalkEvent) -> Bool {
        switch event.state {
        case .refused, .stopped:
            guard outstanding > 0 else { return false }
            outstanding -= 1
            if outstanding > 0 { return false }
        case .starting, .active:
            guard outstanding > 0 else { return false }
        case .other:
            return false
        }
        switch event.state {
        case .starting: phase = .starting
        case .active: phase = .active
        case .refused:
            phase = .refused(reason: event.reason ?? "refused", retryMilliseconds: event.retryMilliseconds)
        case .stopped: phase = .stopped(reason: event.reason ?? "stopped")
        case .other: break
        }
        return true
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
        case "not_permitted": return "not enabled for this camera (Settings > Cameras)"
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
        /// When the phrase was finished (wall clock), for a saved transcript.
        public var at: Date?

        public init(source: TranscriptSource, text: String, isFinal: Bool, at: Date? = nil) {
            self.source = source
            self.text = text
            self.isFinal = isFinal
            self.at = at
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
    /// `at` is when it was heard (wall clock), kept for a saved transcript.
    public mutating func final(_ text: String, source: TranscriptSource, at: Date? = nil) {
        partials[source] = nil
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        finals.append(Line(source: source, text: t, isFinal: true, at: at))
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

/// One doorbell call: from the press (or the user's Start Conversation) to the close or about a minute after the
/// last activity (a press, speech, talk-back).
public struct DoorbellCall: Sendable, Equatable {
    public static let idleTimeout = 60.0

    /// How the call opened.
    public enum Origin: Sendable, Equatable {
        /// A doorbell press.
        case press
        /// The user chose "Start Conversation" on the camera's tile: the same
        /// call (captions, Talk, typed reply) with no press.
        case conversation
    }

    public var origin: Origin

    public var camera: CameraRef
    public var startedAt: Date
    /// When the last press, speech or talk happened, on the viewer clock.
    public var lastActivity: Double
    public var presses: Int
    /// The doorbell plays through the speakers.
    public var audible: Bool
    /// The Unmute control covers the video.
    public var showsUnmute: Bool
    /// Cameras muted for the call; audible again when it ends (unless the
    /// user changed what plays meanwhile).
    public var mutedForCall: Set<CameraRef>
    public var talk = TalkState()
    public var transcript = Transcript()
    /// Tells our own talk-back, heard again by the doorbell's microphone,
    /// from what the visitor says (`caption`).
    public private(set) var echo = CaptionEcho()
    public var transcription: TranscriptionStatus = .preparing
    /// In the viewer windows' insets or in its own window; one state for the
    /// whole app, kept when another press joins the call.
    public var presentation: CallPresentation
    /// "Keep talking" is on: Talk stays on without holding the button. Kept
    /// with the call, so a view built again (a pop-out) shows it.
    public private(set) var latched = false
    /// The typed reply not sent yet, kept with the call for the same reason.
    public var draft = ""

    public init(
        camera: CameraRef, startedAt: Date, now: Double, audio: CallAudio.Decision, origin: Origin = .press,
        presentation: CallPresentation = .inset
    ) {
        self.presentation = presentation
        self.origin = origin
        presses = origin == .press ? 1 : 0
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

    /// A caption from a transcription engine (volatile, or final). Our own
    /// words are remembered for the echo filter; a visitor caption that is
    /// echo of them is dropped (not shown, not saved). Returns whether it
    /// counts as activity: false for echo.
    @discardableResult
    public mutating func caption(
        _ text: String, source: TranscriptSource, final: Bool, now: Double, at: Date? = nil
    ) -> Bool {
        talkChanged(now: now)
        switch source {
        case .you:
            echo.said(text, now: now, final: final)
        case .visitor:
            if echo.filter(text, now: now) {
                transcript.partial("", source: .visitor)
                return false
            }
        }
        if final {
            transcript.final(text, source: source, at: at)
        } else {
            transcript.partial(text, source: source)
        }
        activity(now: now)
        return true
    }

    /// A typed reply is being spoken: the doorbell will play it back.
    public mutating func replySpoken(_ text: String, now: Double) { echo.said(text, now: now) }

    /// Talk-back started, ended or was refused: the echo window follows
    /// `talk.isTalking`. Called after each change of the talk state.
    public mutating func talkChanged(now: Double) { echo.talking(talk.isTalking, now: now) }

    /// Activity keeps the call open.
    public mutating func activity(now: Double) { lastActivity = max(lastActivity, now) }

    /// Another press while the call is open.
    public mutating func pressed(now: Double) {
        presses += 1
        activity(now: now)
    }

    /// "Keep talking" turned on or off by the user (activity).
    public mutating func setLatched(_ on: Bool, now: Double) {
        latched = on
        activity(now: now)
    }

    /// The microphone stopped on its own (refused, the talk ended, no
    /// microphone, the user answered Cancel): "Keep talking" turns off.
    public mutating func microphoneStopped() { latched = false }

    /// The user typed in the reply field (activity).
    public mutating func edited(draft text: String, now: Double) {
        draft = text
        activity(now: now)
    }

    /// A reply was sent: the field empties unless the user typed something
    /// else meanwhile.
    public mutating func sent(_ text: String) {
        if draft == text { draft = "" }
    }

    /// The Unmute control was clicked.
    public mutating func unmuted() {
        audible = true
        showsUnmute = false
    }

    /// Whether the call has ended by itself at `now`: a minute after the
    /// last activity, in the insets only. Talking keeps it open, and a call
    /// in its own window never ends by itself (only when the user closes it,
    /// the device goes away, or the app quits); Pop In gives it a fresh
    /// minute (`activity`).
    public func expired(now: Double) -> Bool {
        presentation == .inset && !talk.isTalking && now - lastActivity >= Self.idleTimeout
    }

    /// The event log's line when it ends; the transcript only when saving is
    /// on.
    public func logText(endedAt: Date, saveTranscript: Bool) -> String {
        let seconds = Int(endedAt.timeIntervalSince(startedAt).rounded())
        var text =
            presses == 0
            ? "Conversation ended: channel \(camera.channel + 1), \(seconds) s"
            : "Doorbell call ended: channel \(camera.channel + 1), \(presses) press\(presses == 1 ? "" : "es"), \(seconds) s"
        if saveTranscript, !transcript.finals.isEmpty { text += "\n" + transcript.savedText }
        return text
    }
}

// MARK: Presentation

/// Where a call is shown (docs/UX.md "Doorbell calls"): in the doorbell's
/// inset in every viewer window, or in a call window of its own.
public enum CallPresentation: String, Sendable, Codable, CaseIterable {
    case inset
    case window

    /// Decided when the call opens (a ring or Start Conversation): the
    /// doorbell's own choice if it has one, else the app's "Open calls in
    /// their own window". A press that joins an open call keeps its
    /// presentation.
    public static func resolve(global openInWindow: Bool, override: CallPresentation?) -> CallPresentation {
        override ?? (openInWindow ? .window : .inset)
    }
}

/// A rectangle in screen points with a top-left origin and y growing
/// downward (Windows' convention; macOS flips with `flipped(height:)`).
public struct WindowRect: Sendable, Equatable, Hashable, Codable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }

    public var minX: Double { x }
    public var minY: Double { y }
    public var maxX: Double { x + width }
    public var maxY: Double { y + height }
    public var midX: Double { x + width / 2 }

    /// Edges included.
    public func contains(x px: Double, y py: Double) -> Bool { px >= minX && px <= maxX && py >= minY && py <= maxY }

    /// The same rectangle with y measured from the other edge of a space
    /// `height` tall (bottom-left origin to top-left and back; macOS uses
    /// the primary screen's height).
    public func flipped(height total: Double) -> WindowRect {
        WindowRect(x: x, y: total - y - height, width: width, height: height)
    }
}

/// Where a call window opens (docs/UX.md "Doorbell calls"). Pure, over the
/// screens' visible frames (the area without menu bar, Dock or taskbar).
public enum CallWindowPlacement {
    /// The "Open call windows" setting.
    public enum Mode: String, Sendable, Codable, CaseIterable {
        /// Where the user last left a call window (the default).
        case remembered
        /// The top right of the screen holding the active viewer window.
        case activeScreen

        public var title: String {
            switch self {
            case .remembered: "Where I left it"
            case .activeScreen: "Top right of the active screen"
            }
        }
    }

    public static let defaultWidth = 480.0
    public static let defaultHeight = 410.0
    /// From the visible frame's edges, for the top right placement.
    public static let margin = 12.0
    /// Each further call window open at once moves this far.
    public static let cascade = 24.0

    /// Whether a remembered frame can still be used: some screen's visible
    /// frame holds the middle of its top edge (the title bar can be reached).
    public static func isOnScreen(_ frame: WindowRect, screens: [WindowRect]) -> Bool {
        screens.contains { $0.contains(x: frame.midX, y: frame.minY) }
    }

    /// The frame for a call window. `remembered`: the last frame the user
    /// left a call window at (its size is kept in both modes); `active`: the
    /// visible frame of the screen holding the key viewer window (nil: the
    /// first screen); `index`: how many call windows are open already.
    ///
    /// "Where I left it" with a remembered frame still on a screen: that
    /// frame; each further window moves 24 pt right and down, kept inside
    /// that screen. Otherwise (the other mode, no frame yet, or its screen is
    /// gone): the top right of the active screen, 12 pt in, each further
    /// window 24 pt left and down, the size limited to the screen.
    public static func frame(
        mode: Mode, remembered: WindowRect?, screens: [WindowRect], active: WindowRect?, index: Int = 0
    ) -> WindowRect {
        let step = cascade * Double(max(index, 0))
        if mode == .remembered, let r = remembered,
            let screen = screens.first(where: { $0.contains(x: r.midX, y: r.minY) })
        {
            guard step > 0 else { return r }
            return clamp(WindowRect(x: r.x + step, y: r.y + step, width: r.width, height: r.height), into: screen)
        }
        let width = remembered?.width ?? defaultWidth
        let height = remembered?.height ?? defaultHeight
        guard let screen = active ?? screens.first else {
            return WindowRect(x: step, y: step, width: width, height: height)
        }
        let w = min(width, max(screen.width - 2 * margin, 0))
        let h = min(height, max(screen.height - 2 * margin, 0))
        let r = WindowRect(x: screen.maxX - margin - w - step, y: screen.minY + margin + step, width: w, height: h)
        return clamp(r, into: screen)
    }

    /// Moved (not resized) to lie inside `screen` where it can.
    static func clamp(_ r: WindowRect, into screen: WindowRect) -> WindowRect {
        let x = max(screen.minX, min(r.x, screen.maxX - r.width))
        let y = max(screen.minY, min(r.y, screen.maxY - r.height))
        return WindowRect(x: x, y: y, width: r.width, height: r.height)
    }
}

/// The call window's stream (docs/UX.md "A call in its own window"): Auto,
/// the main stream while the picture is drawn wider than the sub-stream's
/// pictures, the sub-stream otherwise.
public enum CallWindowQuality {
    /// The sub-stream's width in pixels assumed until one has been seen (the
    /// tile showed the main stream from the start).
    public static let assumedSubWidth = 640.0
    /// Back to the sub-stream only below this share of its width, so a
    /// window resized around the threshold does not switch back and forth.
    public static let downShare = 0.8

    /// Whether the window asks for the main stream, drawing its picture
    /// `drawnWidth` pixels wide; `subWidth` is the sub-stream's picture
    /// width when known, `wasMain` what it asked for last.
    public static func wantsMain(drawnWidth: Double, subWidth: Double?, wasMain: Bool) -> Bool {
        let sub = subWidth.flatMap { $0 > 0 ? $0 : nil } ?? assumedSubWidth
        return wasMain ? drawnWidth >= sub * downShare : drawnWidth > sub
    }
}
