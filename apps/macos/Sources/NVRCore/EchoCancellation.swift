import Foundation

// Echo cancellation for talk-back (docs/AUDIO.md "Echo cancellation"): while
// the microphone is on in a call, the operating system's voice processing
// removes what the computer's own speakers play (the doorbell) from what the
// microphone hears, so the visitor does not hear themselves sent back and the
// user can talk while the visitor is heard. These are the platform-neutral
// rules; NvrView.Tiles (EchoCancellation.cs) has the same rules and the same
// test vectors.

/// What the platform reported when the microphone started.
public struct EchoProbe: Sendable, Equatable {
    /// What the system reports about its echo canceller on the capture stream.
    public enum Effect: Sendable, Equatable {
        /// Present and on.
        case on
        /// Present but turned off (by the user or the driver).
        case off
        /// Not offered by the microphone's processing.
        case absent
        /// The system cannot say (an older system without the query).
        case unknown
    }

    /// The voice-processing (communications) stream was set up.
    public var processing: Bool
    /// Why it was not, in words.
    public var failure: String?
    public var effect: Effect
    /// The echo canceller's reference is the output that plays the doorbell.
    public var referenceIsPlayback: Bool

    public init(processing: Bool, failure: String? = nil, effect: Effect = .unknown, referenceIsPlayback: Bool = false) {
        self.processing = processing
        self.failure = failure
        self.effect = effect
        self.referenceIsPlayback = referenceIsPlayback
    }

    /// The system's voice processing could not be used at all.
    public static func failed(_ why: String) -> EchoProbe { EchoProbe(processing: false, failure: why) }
}

/// Whether the system cancels the doorbell's echo in the microphone, or the
/// app falls back to the caption gate (`MicrophoneEchoGate`).
public enum EchoCancellation: Sendable, Equatable {
    /// The system's echo canceller is on, with the doorbell's output as its reference.
    case system
    /// Not available; why, in words.
    case gated(String)

    /// Only what the system confirms counts: an echo canceller the system
    /// cannot report on is treated as absent.
    public static func decide(_ p: EchoProbe) -> EchoCancellation {
        guard p.processing else { return .gated(p.failure ?? "the system's voice processing did not start") }
        switch p.effect {
        case .absent: return .gated("the microphone has no echo cancellation")
        case .off: return .gated("echo cancellation is turned off for the microphone")
        case .unknown: return .gated("the system does not report echo cancellation")
        case .on: break
        }
        guard p.referenceIsPlayback else { return .gated("echo cancellation follows another output device") }
        return .system
    }

    public var isActive: Bool { self == .system }

    /// The call's status note while the microphone is on (why there is
    /// none, and what it means for the captions, is in the log line).
    public var note: String {
        switch self {
        case .system: "Echo cancellation on"
        case .gated: "No echo cancellation"
        }
    }

    /// The log line (see `EchoCancellationLog`).
    public var logLine: String {
        switch self {
        case .system: "Talk-back microphone: echo cancellation on"
        case .gated(let why): "Talk-back microphone: echo cancellation unavailable (\(why)); captions of the microphone pause while the doorbell plays"
        }
    }
}

/// Logs each distinct outcome once per run, so a fallback that repeats on
/// every press of Talk is not logged every time.
public struct EchoCancellationLog: Sendable {
    private var logged: Set<String> = []

    public init() {}

    /// The line to log for `state`, or nil when it was logged already.
    public mutating func line(for state: EchoCancellation) -> String? {
        logged.insert(state.logLine).inserted ? state.logLine : nil
    }
}

/// Decides when the microphone's own captions pause because the doorbell
/// plays through the speakers. Talk-back itself is never gated: what the
/// microphone hears always goes to the doorbell (echo-cancelled when the
/// system can).
///
/// - Without echo cancellation: the half-duplex gate (`EchoGate`, a peak
///   over 0.03 and 0.4 s after), so the visitor is not captioned twice.
/// - With it: only a short safety duck when the doorbell is very loud (a
///   peak over 0.5, 0.15 s after). A canceller is weakest exactly then: the
///   speakers distort and the echo is no longer a linear copy of the
///   reference, and a residue would be captioned as the user's words.
public struct MicrophoneEchoGate: Sendable, Equatable {
    public static let loudThreshold: Float = 0.5
    public static let loudHold = 0.15

    private var gate = EchoGate()
    private var duck: EchoGate = {
        var g = EchoGate()
        g.threshold = MicrophoneEchoGate.loudThreshold
        g.hold = MicrophoneEchoGate.loudHold
        return g
    }()

    public init() {}

    /// Whether the microphone may be captioned at `now`. Both rules see
    /// every block, so switching between them mid-call keeps their holds.
    public mutating func allowsMicrophone(now: Double, doorbellAudible: Bool, doorbellPeak: Float, echoCancelled: Bool) -> Bool {
        let gated = gate.allowsMicrophone(now: now, doorbellAudible: doorbellAudible, doorbellPeak: doorbellPeak)
        let ducked = duck.allowsMicrophone(now: now, doorbellAudible: doorbellAudible, doorbellPeak: doorbellPeak)
        return echoCancelled ? ducked : gated
    }
}
