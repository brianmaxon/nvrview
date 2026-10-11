import AVFoundation
import Foundation
import NVRCore
import Testing

/// Doorbell calls (docs/UX.md "Doorbell calls"): the audio rules, the talk
/// state machine from `talk` events, the transcript model, the echo gate, the
/// call's end, and the 16 kHz conversion for talk-back.
@Suite struct DoorbellCallTests {
    let door = CameraRef(device: "d-1", channel: 3)
    let yard = CameraRef(device: "d-1", channel: 1)

    // MARK: Audio

    @Test func audibleWhenAlreadyListeningAndTheOtherCameraMutedMeanwhile() {
        #expect(CallAudio.decide(doorbell: door, listening: [yard], unmuteOnRing: false) == .audible(muted: [yard]))
        #expect(CallAudio.decide(doorbell: door, listening: [door], unmuteOnRing: false) == .audible(muted: []))
    }

    @Test func theSettingUnmutesEvenWhenNotListening() {
        #expect(CallAudio.decide(doorbell: door, listening: [], unmuteOnRing: true) == .audible(muted: []))
    }

    @Test func otherwiseTheUnmuteOverlayShows() {
        #expect(CallAudio.decide(doorbell: door, listening: [], unmuteOnRing: false) == .overlay)
        var call = DoorbellCall(camera: door, startedAt: Date(), now: 0, audio: .overlay)
        #expect(call.showsUnmute && !call.audible)
        call.unmuted()
        #expect(!call.showsUnmute && call.audible)
    }

    // MARK: Talk

    @Test func talkGoesThroughStartingToActiveAndStops() {
        var talk = TalkState()
        #expect(talk.phase == .idle)
        // An event with no start of ours is ignored.
        let stray = talk.apply(TalkEvent(tile: 7, state: .active))
        #expect(!stray && talk.phase == .idle)
        let first = talk.requestStart()
        let again = talk.requestStart()
        #expect(first && !again)  // once
        talk.startSent()
        talk.apply(TalkEvent(tile: 7, state: .starting))
        #expect(talk.isTalking && !talk.isActive)
        talk.apply(TalkEvent(tile: 7, state: .active))
        #expect(talk.isActive)
        #expect(talk.text == "Talking")
        talk.apply(TalkEvent(tile: 7, state: .stopped, reason: "stopped", sentMilliseconds: 4096))
        #expect(talk.phase == .stopped(reason: "stopped"))
        #expect(!talk.isTalking)
        #expect(talk.text == "Talk ended")
        let restarted = talk.requestStart()
        #expect(restarted)  // a new start after a stop
    }

    @Test func aLateRefusalOfAnEarlierStartChangesNothing() {
        var talk = TalkState()
        _ = talk.requestStart()
        talk.startFailed(.notPermitted)  // its refused event is still on the way
        // The user allowed talk-back and pressed Talk again.
        _ = talk.requestStart()
        talk.startSent()
        let late = talk.apply(TalkEvent(tile: 7, state: .refused, reason: "not_permitted"))
        #expect(!late)
        #expect(talk.phase == .requested)
        talk.apply(TalkEvent(tile: 7, state: .starting))
        talk.apply(TalkEvent(tile: 7, state: .active))
        #expect(talk.isActive)
        talk.apply(TalkEvent(tile: 7, state: .stopped, reason: "stopped"))
        #expect(talk.phase == .stopped(reason: "stopped"))
        #expect(talk.outstanding == 0)
    }

    @Test func notPermittedIsRefusedWithAReason() {
        var talk = TalkState()
        let asked = talk.requestStart()
        #expect(asked)
        talk.startFailed(.notPermitted)
        #expect(talk.phase == .refused(reason: "not_permitted", retryMilliseconds: nil))
        #expect(talk.text.contains("not enabled for this doorbell"))
        // The event that comes with it says the same.
        talk.apply(TalkEvent(tile: 7, state: .refused, reason: "not_permitted"))
        #expect(!talk.isTalking)
    }

    @Test func refusalsSayWhy() {
        var talk = TalkState()
        _ = talk.requestStart()
        talk.startSent()
        talk.apply(TalkEvent(tile: 7, state: .refused, reason: "rate_limited", retryMilliseconds: 41_000))
        #expect(talk.text == "Talk-back refused: too many attempts (try again in 41 s)")
        _ = talk.requestStart()
        talk.startSent()
        talk.apply(TalkEvent(tile: 7, state: .stopped, reason: "idle"))
        #expect(talk.text == "Talk ended: no audio for a while")
        #expect(TalkState.words("device_refused") == "the device refused")
    }

    @Test func lettingGoBeforeAnyEventReturnsToIdle() {
        var talk = TalkState()
        _ = talk.requestStart()
        talk.stopRequested()
        #expect(talk.phase == .idle)
    }

    // MARK: Transcript

    @Test func partialsAreReplacedAndFinalsAppended() {
        var t = Transcript()
        t.partial("hel", source: .visitor)
        t.partial("hello the", source: .visitor)
        #expect(t.lines == [.init(source: .visitor, text: "hello the", isFinal: false)])
        t.partial("one moment", source: .you)
        t.final("Hello there.", source: .visitor)
        #expect(
            t.lines == [
                .init(source: .visitor, text: "Hello there.", isFinal: true),
                .init(source: .you, text: "one moment", isFinal: false),
            ])
        t.final("One moment please.", source: .you)
        t.final("  ", source: .visitor)
        #expect(t.finals.count == 2)
        #expect(t.partials.isEmpty)
        #expect(t.savedText == "Visitor: Hello there.\nYou: One moment please.")
        #expect(TranscriptSource.visitor.label == "Visitor" && TranscriptSource.you.label == "You")
    }

    // MARK: Echo

    @Test func theMicrophoneIsGatedWhileTheDoorbellPlays() {
        var gate = EchoGate()
        let r1 = gate.allowsMicrophone(now: 0, doorbellAudible: true, doorbellPeak: 0)
        #expect(r1)
        let r2 = gate.allowsMicrophone(now: 1, doorbellAudible: true, doorbellPeak: 0.4)
        #expect(!r2)
        let r3 = gate.allowsMicrophone(now: 1.3, doorbellAudible: true, doorbellPeak: 0.01)
        #expect(!r3)
        let r4 = gate.allowsMicrophone(now: 1.41, doorbellAudible: true, doorbellPeak: 0.01)
        #expect(r4)
        // Muted doorbell: nothing comes out of the speakers to echo.
        let r5 = gate.allowsMicrophone(now: 2, doorbellAudible: false, doorbellPeak: 0.9)
        #expect(r5)
    }

    // MARK: The call

    @Test func theCallEndsAMinuteAfterTheLastActivityUnlessTalking() {
        var call = DoorbellCall(camera: door, startedAt: Date(timeIntervalSince1970: 0), now: 0, audio: .audible(muted: [yard]))
        #expect(call.mutedForCall == [yard])
        #expect(!call.expired(now: 59))
        call.pressed(now: 30)
        #expect(!call.expired(now: 89))
        #expect(call.expired(now: 90))
        _ = call.talk.requestStart()
        #expect(!call.expired(now: 500))
        call.transcript.final("Hello.", source: .visitor)
        #expect(call.presses == 2)
        let unsaved = call.logText(endedAt: Date(timeIntervalSince1970: 95), saveTranscript: false)
        #expect(unsaved == "Doorbell call ended: channel 4, 2 presses, 95 s")
        #expect(call.logText(endedAt: Date(timeIntervalSince1970: 95), saveTranscript: true).hasSuffix("\nVisitor: Hello."))
    }

    // MARK: Conversation without a press

    @Test func aConversationIsAudibleAndMutesWhatElsePlayed() {
        #expect(CallAudio.decideConversation(doorbell: door, listening: []) == .audible(muted: []))
        #expect(CallAudio.decideConversation(doorbell: door, listening: [yard, door]) == .audible(muted: [yard]))
    }

    @Test func aConversationIsTheSameCallWithNoPress() {
        var call = DoorbellCall(camera: door, startedAt: Date(timeIntervalSince1970: 0), now: 0, audio: .audible(muted: []), origin: .conversation)
        #expect(call.origin == .conversation && call.presses == 0)
        #expect(call.audible && !call.showsUnmute)
        // Idle for a minute ends it; talking keeps it open, as for a press.
        #expect(!call.expired(now: 59) && call.expired(now: 60))
        _ = call.talk.requestStart()
        #expect(!call.expired(now: 500))
        call.talk.stopRequested()
        call.transcript.final("Hello.", source: .visitor)
        let text = call.logText(endedAt: Date(timeIntervalSince1970: 42), saveTranscript: false)
        #expect(text == "Conversation ended: channel 4, 42 s")
        #expect(call.logText(endedAt: Date(timeIntervalSince1970: 42), saveTranscript: true).hasSuffix("\nVisitor: Hello."))
        // A press during the conversation counts, and the log says so.
        call.pressed(now: 5)
        #expect(call.logText(endedAt: Date(timeIntervalSince1970: 42), saveTranscript: false) == "Doorbell call ended: channel 4, 1 press, 42 s")
    }

    // MARK: 16 kHz conversion

    private func sine(rate: Double, channels: AVAudioChannelCount, seconds: Double, hz: Double) -> AVAudioPCMBuffer {
        let format = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: rate, channels: channels, interleaved: false)!
        let frames = AVAudioFrameCount(rate * seconds)
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)!
        buffer.frameLength = frames
        for c in 0..<Int(channels) {
            let data = buffer.int16ChannelData![c]
            for i in 0..<Int(frames) { data[i] = Int16(12_000 * sin(2 * Double.pi * hz * Double(i) / rate)) }
        }
        return buffer
    }

    /// Zero crossings per second, about twice the frequency.
    private func frequency(_ s: [Float], rate: Double) -> Double {
        var crossings = 0
        for i in 1..<s.count where (s[i - 1] < 0) != (s[i] < 0) { crossings += 1 }
        return Double(crossings) / 2 / (Double(s.count) / rate)
    }

    @Test func speechRenderedAt22kHzBecomes16kHzMonoFloat() {
        // AVSpeechSynthesizer renders 16-bit mono at 22.05 kHz; in two parts.
        let whole = sine(rate: 22_050, channels: 1, seconds: 1, hz: 440)
        let first = sine(rate: 22_050, channels: 1, seconds: 0.5, hz: 440)
        let samples = TalkAudioConverter.convertAll([first, first])
        #expect(abs(samples.count - 16_000) <= 2)
        #expect(abs(frequency(samples, rate: 16_000) - 440) < 5)
        let peak = samples.map(abs).max() ?? 0
        #expect(abs(peak - 12_000 / 32_768) < 0.03)
        #expect(abs(TalkAudioConverter.convertAll([whole]).count - 16_000) <= 2)
    }

    @Test func aStereo48kHzMicrophoneBufferIsMixedDown() {
        let mic = sine(rate: 48_000, channels: 2, seconds: 0.25, hz: 1000)
        let c = TalkAudioConverter()
        var samples = c.convert(mic)
        samples += c.convert(mic)
        samples += c.flush()
        #expect(abs(samples.count - 8_000) <= 2)
        #expect(abs(frequency(samples, rate: 16_000) - 1000) < 10)
        #expect((samples.map(abs).max() ?? 0) > 0.2)
    }
}
