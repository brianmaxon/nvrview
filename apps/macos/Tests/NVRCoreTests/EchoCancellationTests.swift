import NVRCore
import Testing

/// Echo cancellation for talk-back (docs/AUDIO.md "Echo cancellation"): when
/// the system's canceller counts as on, and the caption gate in each mode.
/// NvrView.Tiles.Tests (EchoCancellationTests.cs) has the same vectors.
@Suite struct EchoCancellationTests {
    @Test func onlyAConfirmedCancellerOnThePlaybackOutputCounts() {
        #expect(EchoCancellation.decide(EchoProbe(processing: true, effect: .on, referenceIsPlayback: true)) == .system)
        #expect(EchoCancellation.decide(.failed("no input device")) == .gated("no input device"))
        #expect(EchoCancellation.decide(EchoProbe(processing: false)) == .gated("the system's voice processing did not start"))
        #expect(
            EchoCancellation.decide(EchoProbe(processing: true, effect: .absent, referenceIsPlayback: true))
                == .gated("the microphone has no echo cancellation"))
        #expect(
            EchoCancellation.decide(EchoProbe(processing: true, effect: .off, referenceIsPlayback: true))
                == .gated("echo cancellation is turned off for the microphone"))
        #expect(
            EchoCancellation.decide(EchoProbe(processing: true, effect: .unknown, referenceIsPlayback: true))
                == .gated("the system does not report echo cancellation"))
        #expect(
            EchoCancellation.decide(EchoProbe(processing: true, effect: .on, referenceIsPlayback: false))
                == .gated("echo cancellation follows another output device"))
    }

    @Test func theNoteAndTheLogSayWhich() {
        #expect(EchoCancellation.system.note == "Echo cancellation on")
        #expect(EchoCancellation.gated("x").note == "No echo cancellation")
        #expect(EchoCancellation.system.logLine == "Talk-back microphone: echo cancellation on")
        #expect(
            EchoCancellation.gated("no input device").logLine
                == "Talk-back microphone: echo cancellation unavailable (no input device); captions of the microphone pause while the doorbell plays")
        #expect(EchoCancellation.system.isActive && !EchoCancellation.gated("x").isActive)
    }

    @Test func eachOutcomeIsLoggedOnce() {
        var log = EchoCancellationLog()
        func line(_ state: EchoCancellation) -> String? { log.line(for: state) }
        #expect(line(.gated("a")) != nil)
        #expect(line(.gated("a")) == nil)
        #expect(line(.gated("b")) != nil)
        #expect(line(.system) != nil)
        #expect(line(.system) == nil)
    }

    @Test func withoutCancellationTheCaptionsPauseWhileTheDoorbellPlays() {
        var gate = MicrophoneEchoGate()
        func open(_ now: Double, _ audible: Bool, _ peak: Float, _ cancelled: Bool) -> Bool {
            gate.allowsMicrophone(now: now, doorbellAudible: audible, doorbellPeak: peak, echoCancelled: cancelled)
        }
        #expect(open(0, true, 0.01, false))
        #expect(!open(1, true, 0.2, false))
        #expect(!open(1.3, true, 0, false))
        #expect(open(1.41, true, 0, false))
        #expect(open(2, false, 0.9, false))
    }

    @Test func withCancellationOnlyAVeryLoudDoorbellDucksBriefly() {
        var gate = MicrophoneEchoGate()
        func open(_ now: Double, _ audible: Bool, _ peak: Float, _ cancelled: Bool) -> Bool {
            gate.allowsMicrophone(now: now, doorbellAudible: audible, doorbellPeak: peak, echoCancelled: cancelled)
        }
        // Ordinary speech from the doorbell: full duplex.
        #expect(open(0, true, 0.3, true))
        #expect(open(0.1, true, 0.5, true))
        // Very loud: a short duck.
        #expect(!open(1, true, 0.8, true))
        #expect(!open(1.14, true, 0.1, true))
        #expect(open(1.16, true, 0.1, true))
        // Muted doorbell: nothing to duck.
        #expect(open(2, false, 0.9, true))
    }

    @Test func losingCancellationMidCallKeepsTheGatesHold() {
        var gate = MicrophoneEchoGate()
        func open(_ now: Double, _ audible: Bool, _ peak: Float, _ cancelled: Bool) -> Bool {
            gate.allowsMicrophone(now: now, doorbellAudible: audible, doorbellPeak: peak, echoCancelled: cancelled)
        }
        #expect(open(0, true, 0.2, true))
        // Voice processing went away a moment later: the gate already holds.
        #expect(!open(0.2, true, 0, false))
        #expect(open(0.41, true, 0, false))
    }
}
