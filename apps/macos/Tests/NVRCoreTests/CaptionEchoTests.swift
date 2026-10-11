import Foundation
import NVRCore
import Testing

/// The caption echo filter (docs/UX.md "Captions"). The vectors are mirrored
/// in apps/windows/NvrView.Tiles.Tests/CaptionEchoTests.cs.
@Suite struct CaptionEchoTests {
    private func said(_ text: String, at now: Double = 10, final: Bool = true) -> CaptionEcho {
        var e = CaptionEcho()
        e.said(text, now: now, final: final)
        return e
    }

    @Test func aGarbledEchoOfASpokenPhraseIsEcho() {
        let e = said("Hello, this is a test.")
        #expect(e.isEcho("Hello, this is the test.", now: 11))
    }

    @Test func aFragmentOfATypedReplyIsEcho() {
        let e = said("Hello this is a text to speech test")
        #expect(e.isEcho("Hello, this is a text.", now: 11.5))
        #expect(e.isEcho("Hello, this is", now: 11))
    }

    @Test func unrelatedVisitorSpeechIsNeverEcho() {
        let e = said("Hello, this is a test.")
        #expect(!e.isEcho("Who is it? I have a package for you.", now: 11))
        #expect(!e.isEcho("Yes this is the delivery", now: 11))
        #expect(!e.isEcho("Hello", now: 11))
    }

    @Test func nothingSaidMeansNothingIsEcho() {
        let e = CaptionEcho()
        #expect(!e.isEcho("Hello, this is a test.", now: 1))
    }

    @Test func outsideTheWindowIsNotEcho() {
        let e = said("Hello, this is a test.")
        #expect(e.isEcho("Hello, this is a test.", now: 10 + CaptionEcho.window - 0.1))
        #expect(!e.isEcho("Hello, this is a test.", now: 10 + CaptionEcho.window + 0.1))
    }

    @Test func theWindowRunsFromTheEndOfTalk() {
        var e = said("Hello, this is a test.")
        e.talking(true, now: 10)
        #expect(e.isEcho("Hello this is a test", now: 30))
        e.talking(false, now: 20)
        let closes = 20 + CaptionEcho.window
        #expect(e.isEcho("Hello this is a test", now: closes - 0.1))
        #expect(!e.isEcho("Hello this is a test", now: closes + 0.1))
    }

    @Test func numberWordsMatchDigits() {
        let e = said("one two three four five six seven")
        #expect(e.isEcho("1 2 3 4 5 6 7", now: 11))
        let f = said("call 21 now")
        #expect(f.isEcho("call twenty one now", now: 11))
    }

    @Test func aVolatilePhraseInProgressCountsAndTheFinalReplacesIt() {
        var e = CaptionEcho()
        e.said("hello this is", now: 10, final: false)
        #expect(e.isEcho("hello this is", now: 11))
        e.said("Goodbye for now", now: 12, final: true)
        #expect(!e.isEcho("hello this is", now: 13))
        #expect(e.isEcho("goodbye for now", now: 13))
    }

    @Test func phrasesSaidInARowCanComeBackAsOneCaption() {
        var e = CaptionEcho()
        e.said("Hello there", now: 10)
        e.said("how can I help", now: 11)
        #expect(e.isEcho("hello there how can I help", now: 12))
    }

    @Test func filterCountsDrops() {
        var e = said("Hello, this is a test.")
        let first = e.filter("hello this is a test", now: 11)
        let second = e.filter("who is there", now: 11)
        #expect(first)
        #expect(!second)
        #expect(e.dropped == 1)
    }

    @Test func aCallDropsEchoCaptionsAndDoesNotCountThemAsActivity() {
        var call = DoorbellCall(
            camera: CameraRef(device: "d-1", channel: 3), startedAt: Date(timeIntervalSince1970: 0), now: 0, audio: .overlay)
        call.replySpoken("Hello, this is a test.", now: 10)
        let kept = call.caption("Who is it", source: .visitor, final: true, now: 11)
        #expect(kept)
        #expect(call.lastActivity == 11)
        call.caption("Hello, this is", source: .visitor, final: false, now: 12)
        #expect(call.transcript.partials[.visitor] == nil)
        let echoed = call.caption("Hello, this is the test.", source: .visitor, final: true, now: 13)
        #expect(!echoed)
        #expect(call.lastActivity == 11)
        #expect(call.transcript.finals.map(\.text) == ["Who is it"])
        #expect(call.echo.dropped == 2)
        // Past the window the same words are the visitor's.
        let later = call.caption("Hello, this is the test.", source: .visitor, final: true, now: 14)
        #expect(later)
        #expect(call.transcript.finals.count == 2)
    }
}
