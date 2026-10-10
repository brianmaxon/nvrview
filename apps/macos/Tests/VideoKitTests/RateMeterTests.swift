import Foundation
import Testing
import VideoKit

@Suite struct RateMeterTests {
    private func feed(_ m: inout RateMeter, fps: Double, seconds: Double, from start: Int64 = 0) -> Int64 {
        let step = Int64(1_000_000 / fps)
        var t = start
        for _ in 0..<Int(fps * seconds) {
            m.add(t)
            t += step
        }
        return t - step
    }

    @Test func measuresASteadyRate() throws {
        var m = RateMeter()
        #expect(m.rate() == nil)
        m.add(0)
        #expect(m.rate() == nil)
        _ = feed(&m, fps: 15, seconds: 4)
        #expect(abs(try #require(m.rate()) - 15) < 0.05)
    }

    @Test func followsAChangeWithinTheWindow() throws {
        var m = RateMeter(windowMicros: 2_000_000)
        let end = feed(&m, fps: 25, seconds: 5)
        _ = feed(&m, fps: 10, seconds: 3, from: end + 100_000)
        // Only the last two seconds count.
        #expect(abs(try #require(m.rate()) - 10) < 0.2)
    }

    @Test func aHighRateDoesNotOverflowTheRing() throws {
        var m = RateMeter(capacity: 240)
        _ = feed(&m, fps: 240, seconds: 3)
        #expect(abs(try #require(m.rate()) - 240) < 2)
    }

    @Test func aWrapOrRestartStartsOver() throws {
        var m = RateMeter()
        let end = feed(&m, fps: 20, seconds: 3, from: 4_294_000_000)
        // Timestamps wrap to near zero: no negative or huge rate.
        m.add(1_000)
        #expect(m.rate() == nil)
        _ = feed(&m, fps: 20, seconds: 2, from: 51_000)
        #expect(abs(try #require(m.rate()) - 20) < 0.2)
        // A jump forward far beyond the gap also starts over.
        m.add(end + 60_000_000)
        #expect(m.rate() == nil)
    }

    @Test func aPictureSlightlyOutOfOrderIsIgnored() throws {
        var m = RateMeter()
        let end = feed(&m, fps: 20, seconds: 2)
        m.add(end - 50_000)
        m.add(end)
        #expect(abs(try #require(m.rate()) - 20) < 0.2)
    }

    @Test func aStoppedStreamReadsNothing() {
        var m = RateMeter()
        let end = feed(&m, fps: 20, seconds: 2)
        #expect(m.rate(now: end + 500_000) != nil)
        #expect(m.rate(now: end + 2_500_000) == nil)
    }
}
